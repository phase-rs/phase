use crate::game::coverage::{CardCoverageResult, CoverageSummary};
use crate::parser::oracle_effect::normalize_verb_token;
use crate::parser::oracle_effect::subject::{starts_with_subject_prefix, PREDICATE_VERBS};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

// ── Recognized verbs ────────────────────────────────────────────────────────
//
// Union of three sources (see plan for rationale):
// A) PREDICATE_VERBS from subject.rs (37 verbs used for subject-predicate splitting)
// B) Additional first-word verbs from parse_imperative_family_ast match arms
// C) Pre-dispatch verbs from parse_effect_clause and lower_imperative_clause
//
// NOTE: when adding verbs to parse_imperative_family_ast, also add them here.

/// Additional verbs from `parse_imperative_family_ast` not in `PREDICATE_VERBS`.
const IMPERATIVE_EXTRA_VERBS: &[&str] = &[
    "spend",
    "double",
    "destroy",
    "prevent",
    "attach",
    "seek",
    "amass",
    "monstrosity",
    "flip",
    "roll",
    "note",
    "manifest",
    "investigate",
    "proliferate",
    "suspect",
    "blight",
    "forage",
    "collect",
    "endure",
    "goad",
    "exchange",
    "must",
    "earthbend",
    "airbend",
    "bounce",
    "equip",
    "remove",
    "switch",
    "populate",
    "clash",
];

/// Pre-dispatch verbs handled in `parse_effect_clause` before imperative dispatch.
const PRE_DISPATCH_VERBS: &[&str] = &[
    "tempt",      // "the ring tempts you"
    "discover",   // "discover N"
    "distribute", // "distribute N counters among"
];

/// Keywords/mechanics known to be unimplemented in the engine.
const NEW_MECHANIC_KEYWORDS: &[&str] = &[
    "conjure",
    "specialize",
    "specializes",
    "perpetually",
    "seek", // Alchemy-only digital verb (different from search)
    "draft",
    "drafted",
    "ante",
    "augment",
    "sticker",
    "attraction",
    "unfinity",
    "conspiracy",
    "scheme",
    "vanguard",
    "dungeon",
];

fn is_recognized_verb(verb: &str) -> bool {
    let normalized = normalize_verb_token(verb);
    let n = normalized.as_str();
    PREDICATE_VERBS.contains(&n)
        || IMPERATIVE_EXTRA_VERBS.contains(&n)
        || PRE_DISPATCH_VERBS.contains(&n)
}

fn contains_new_mechanic_keyword(text: &str) -> bool {
    let lower = text.to_lowercase();
    NEW_MECHANIC_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

// ── Classification types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapCategory {
    /// Text contains a verb the parser handles, but specific pattern failed.
    VerbVariation,
    /// Subject phrase not caught by `starts_with_subject_prefix`, but predicate verb is handled.
    SubjectStripping,
    /// Trigger mode registered, but execute effect inside it is unimplemented.
    TriggerEffect,
    /// Static mode registered, but condition text is unrecognized.
    StaticCondition,
    /// Genuinely new mechanic not in the engine.
    NewMechanic,
    /// Doesn't fit other categories.
    Unclassified,
}

impl GapCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::VerbVariation => "A_verb_variation",
            Self::SubjectStripping => "B_subject_stripping",
            Self::TriggerEffect => "C_trigger_effect",
            Self::StaticCondition => "D_static_condition",
            Self::NewMechanic => "F_new_mechanic",
            Self::Unclassified => "G_unclassified",
        }
    }

    pub fn is_near_miss(&self) -> bool {
        matches!(
            self,
            Self::VerbVariation
                | Self::SubjectStripping
                | Self::TriggerEffect
                | Self::StaticCondition
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassifiedGap {
    pub handler: String,
    pub source_text: Option<String>,
    pub category: GapCategory,
    /// For VerbVariation, the recognized verb that was detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_verb: Option<String>,
    /// Whether the verb was found at a non-initial position.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub non_initial_verb: Option<bool>,
    pub card_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerbBreakdown {
    pub verb: String,
    pub count: usize,
    pub single_gap_unlocks: usize,
    pub top_patterns: Vec<PatternEntry>,
    pub example_cards: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatternEntry {
    pub pattern: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategorySummary {
    pub count: usize,
    pub single_gap_unlocks: usize,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub by_verb: Vec<VerbBreakdown>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub top_patterns: Vec<PatternEntry>,
    pub example_cards: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuickWin {
    pub description: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verb: Option<String>,
    pub cards_unlocked: usize,
    pub pattern: String,
    pub example_cards: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GapAnalysis {
    pub analysis_date: String,
    pub total_unsupported: usize,
    pub total_classified: usize,
    pub categories: BTreeMap<String, CategorySummary>,
    pub quick_wins: Vec<QuickWin>,
}

// ── Classification logic ────────────────────────────────────────────────────

fn classify_gap(
    handler: &str,
    source_text: Option<&str>,
    card_gaps: &[String],
) -> (GapCategory, Option<String>, Option<bool>) {
    // Static condition gaps
    if handler.starts_with("Static:Unrecognized") {
        return (GapCategory::StaticCondition, None, None);
    }

    // Trigger gaps where the trigger mode is registered but execute effect failed:
    // detected by checking if this card also has Effect: gaps co-occurring with
    // a non-Unknown trigger gap
    if handler.starts_with("Trigger:") && !handler.contains("Unknown(") {
        let has_effect_gap = card_gaps
            .iter()
            .any(|g| g.starts_with("Effect:") && g != handler);
        if has_effect_gap {
            return (GapCategory::TriggerEffect, None, None);
        }
    }

    let Some(text) = source_text else {
        return (GapCategory::Unclassified, None, None);
    };

    let lower = text.to_lowercase();
    let lower = lower.trim();

    if lower.is_empty() {
        return (GapCategory::Unclassified, None, None);
    }

    // Check for new mechanic keywords first
    if contains_new_mechanic_keyword(lower) {
        return (GapCategory::NewMechanic, None, None);
    }

    // Category A: first word is a recognized verb
    if let Some(first_word) = lower.split_whitespace().next() {
        let normalized = normalize_verb_token(first_word);
        if is_recognized_verb(&normalized) {
            return (GapCategory::VerbVariation, Some(normalized), Some(false));
        }
    }

    // Category B: subject prefix present, and predicate verb after subject is recognized.
    // Checked before non-initial verb (A) because subject stripping is a more specific
    // and actionable classification — a single fix in subject.rs vs hunting verb handlers.
    if starts_with_subject_prefix(lower) {
        for word in lower.split_whitespace().skip(1) {
            let normalized = normalize_verb_token(word);
            if is_recognized_verb(&normalized) {
                return (GapCategory::SubjectStripping, Some(normalized), None);
            }
        }
    }

    // Category A (non-initial): text contains a recognized verb at non-initial position
    for word in lower.split_whitespace().skip(1) {
        let normalized = normalize_verb_token(word);
        if is_recognized_verb(&normalized) {
            return (GapCategory::VerbVariation, Some(normalized), Some(true));
        }
    }

    (GapCategory::Unclassified, None, None)
}

/// Analyze a coverage summary to classify each gap by failure reason.
pub fn analyze_gaps(summary: &CoverageSummary) -> GapAnalysis {
    let today = String::new(); // Set by the binary caller

    let unsupported_cards: Vec<&CardCoverageResult> =
        summary.cards.iter().filter(|c| !c.supported).collect();

    let mut classified: Vec<ClassifiedGap> = Vec::new();

    for card in &unsupported_cards {
        let card_gap_handlers: Vec<String> =
            card.gap_details.iter().map(|g| g.handler.clone()).collect();

        for gap in &card.gap_details {
            // Skip "Effect:empty" — these are empty clauses after connector stripping
            if gap.handler == "Effect:empty" {
                continue;
            }

            let (category, matched_verb, non_initial) =
                classify_gap(&gap.handler, gap.source_text.as_deref(), &card_gap_handlers);

            classified.push(ClassifiedGap {
                handler: gap.handler.clone(),
                source_text: gap.source_text.clone(),
                category,
                matched_verb,
                non_initial_verb: non_initial,
                card_name: card.card_name.clone(),
            });
        }
    }

    // Build single-gap card set for unlock counting
    let single_gap_cards: HashMap<&str, GapCategory> = unsupported_cards
        .iter()
        .filter(|c| c.gap_count == 1)
        .filter_map(|c| {
            let gap = c.gap_details.first()?;
            if gap.handler == "Effect:empty" {
                return None;
            }
            let card_gap_handlers: Vec<String> = vec![gap.handler.clone()];
            let (cat, _, _) =
                classify_gap(&gap.handler, gap.source_text.as_deref(), &card_gap_handlers);
            Some((c.card_name.as_str(), cat))
        })
        .collect();

    // Aggregate by category
    let mut category_data: BTreeMap<GapCategory, Vec<&ClassifiedGap>> = BTreeMap::new();
    for gap in &classified {
        category_data.entry(gap.category).or_default().push(gap);
    }

    let mut categories = BTreeMap::new();

    for (cat, gaps) in &category_data {
        let count = gaps.len();
        let single_gap_count = gaps
            .iter()
            .filter(|g| single_gap_cards.get(g.card_name.as_str()) == Some(cat))
            .map(|g| &g.card_name)
            .collect::<std::collections::HashSet<_>>()
            .len();

        // Build verb breakdown for VerbVariation and SubjectStripping
        let by_verb = if matches!(
            cat,
            GapCategory::VerbVariation | GapCategory::SubjectStripping
        ) {
            let mut verb_groups: BTreeMap<String, Vec<&ClassifiedGap>> = BTreeMap::new();
            for gap in gaps {
                if let Some(verb) = &gap.matched_verb {
                    verb_groups.entry(verb.clone()).or_default().push(gap);
                }
            }

            let mut breakdowns: Vec<VerbBreakdown> = verb_groups
                .into_iter()
                .map(|(verb, verb_gaps)| {
                    let verb_count = verb_gaps.len();
                    let verb_single = verb_gaps
                        .iter()
                        .filter(|g| single_gap_cards.get(g.card_name.as_str()) == Some(cat))
                        .map(|g| &g.card_name)
                        .collect::<std::collections::HashSet<_>>()
                        .len();

                    // Aggregate patterns
                    let mut pattern_counts: HashMap<String, usize> = HashMap::new();
                    for g in &verb_gaps {
                        if let Some(text) = &g.source_text {
                            let pattern = normalize_gap_pattern(text);
                            *pattern_counts.entry(pattern).or_default() += 1;
                        }
                    }
                    let mut top_patterns: Vec<PatternEntry> = pattern_counts
                        .into_iter()
                        .map(|(pattern, count)| PatternEntry { pattern, count })
                        .collect();
                    top_patterns.sort_by_key(|p| std::cmp::Reverse(p.count));
                    top_patterns.truncate(10);

                    let example_cards: Vec<String> = verb_gaps
                        .iter()
                        .map(|g| g.card_name.clone())
                        .collect::<std::collections::HashSet<_>>()
                        .into_iter()
                        .take(5)
                        .collect();

                    VerbBreakdown {
                        verb,
                        count: verb_count,
                        single_gap_unlocks: verb_single,
                        top_patterns,
                        example_cards,
                    }
                })
                .collect();
            breakdowns.sort_by_key(|b| std::cmp::Reverse(b.single_gap_unlocks));
            breakdowns
        } else {
            vec![]
        };

        // Top patterns for non-verb categories
        let top_patterns = if by_verb.is_empty() {
            let mut pattern_counts: HashMap<String, usize> = HashMap::new();
            for g in gaps {
                if let Some(text) = &g.source_text {
                    let pattern = normalize_gap_pattern(text);
                    *pattern_counts.entry(pattern).or_default() += 1;
                }
            }
            let mut patterns: Vec<PatternEntry> = pattern_counts
                .into_iter()
                .map(|(pattern, count)| PatternEntry { pattern, count })
                .collect();
            patterns.sort_by_key(|p| std::cmp::Reverse(p.count));
            patterns.truncate(20);
            patterns
        } else {
            vec![]
        };

        let example_cards: Vec<String> = gaps
            .iter()
            .map(|g| g.card_name.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .take(10)
            .collect();

        categories.insert(
            cat.label().to_string(),
            CategorySummary {
                count,
                single_gap_unlocks: single_gap_count,
                by_verb,
                top_patterns,
                example_cards,
            },
        );
    }

    // Build quick wins from highest-impact verb breakdowns
    let mut quick_wins: Vec<QuickWin> = Vec::new();
    for (cat_label, cat_summary) in &categories {
        for verb_bd in &cat_summary.by_verb {
            if verb_bd.single_gap_unlocks > 0 {
                let top_pattern = verb_bd
                    .top_patterns
                    .first()
                    .map(|p| p.pattern.clone())
                    .unwrap_or_default();
                quick_wins.push(QuickWin {
                    description: format!(
                        "Handle '{}' variation: \"{}\" ({} cards)",
                        verb_bd.verb, top_pattern, verb_bd.single_gap_unlocks
                    ),
                    category: cat_label.clone(),
                    verb: Some(verb_bd.verb.clone()),
                    cards_unlocked: verb_bd.single_gap_unlocks,
                    pattern: top_pattern,
                    example_cards: verb_bd.example_cards.clone(),
                });
            }
        }
        // For non-verb categories with single-gap unlocks
        if cat_summary.by_verb.is_empty() && cat_summary.single_gap_unlocks > 0 {
            let top_pattern = cat_summary
                .top_patterns
                .first()
                .map(|p| p.pattern.clone())
                .unwrap_or_default();
            quick_wins.push(QuickWin {
                description: format!(
                    "{}: \"{}\" ({} cards)",
                    cat_label, top_pattern, cat_summary.single_gap_unlocks
                ),
                category: cat_label.clone(),
                verb: None,
                cards_unlocked: cat_summary.single_gap_unlocks,
                pattern: top_pattern,
                example_cards: cat_summary.example_cards.iter().take(5).cloned().collect(),
            });
        }
    }
    quick_wins.sort_by_key(|w| std::cmp::Reverse(w.cards_unlocked));
    quick_wins.truncate(30);

    GapAnalysis {
        analysis_date: today,
        total_unsupported: unsupported_cards.len(),
        total_classified: classified.len(),
        categories,
        quick_wins,
    }
}

/// Simplified pattern normalization for gap text — lowercases and normalizes
/// card-specific details while preserving the structural verb + pattern.
fn normalize_gap_pattern(text: &str) -> String {
    let s = text.to_lowercase();
    let s = s.trim_end_matches('.');
    // Collapse multiple spaces
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognized_verbs_cover_predicate_verbs() {
        for verb in PREDICATE_VERBS {
            assert!(
                is_recognized_verb(verb),
                "PREDICATE_VERB '{}' not recognized",
                verb
            );
        }
    }

    #[test]
    fn recognized_verbs_cover_imperative_extras() {
        for verb in IMPERATIVE_EXTRA_VERBS {
            assert!(
                is_recognized_verb(verb),
                "IMPERATIVE_EXTRA_VERB '{}' not recognized",
                verb
            );
        }
    }

    #[test]
    fn deconjugated_verbs_recognized() {
        assert!(is_recognized_verb("destroys"));
        assert!(is_recognized_verb("draws"));
        assert!(is_recognized_verb("creates"));
        assert!(is_recognized_verb("has")); // → "have"
        assert!(is_recognized_verb("copies")); // → "copy"
    }

    #[test]
    fn classify_verb_variation_first_word() {
        let (cat, verb, non_initial) = classify_gap(
            "Effect:destroy",
            Some("destroy each creature with flying"),
            &[],
        );
        assert_eq!(cat, GapCategory::VerbVariation);
        assert_eq!(verb.as_deref(), Some("destroy"));
        assert_eq!(non_initial, Some(false));
    }

    #[test]
    fn classify_verb_variation_non_initial() {
        let (cat, verb, non_initial) =
            classify_gap("Effect:unknown", Some("Lightning Bolt deals 3 damage"), &[]);
        assert_eq!(cat, GapCategory::VerbVariation);
        assert_eq!(verb.as_deref(), Some("deal"));
        assert_eq!(non_initial, Some(true));
    }

    #[test]
    fn classify_subject_stripping() {
        let (cat, verb, _) = classify_gap("Effect:that", Some("that player discards a card"), &[]);
        // "that" starts_with_subject_prefix, "discards" → "discard" is recognized
        assert_eq!(cat, GapCategory::SubjectStripping);
        assert_eq!(verb.as_deref(), Some("discard"));
    }

    #[test]
    fn classify_new_mechanic() {
        let (cat, _, _) = classify_gap(
            "Effect:conjure",
            Some("conjure a duplicate of target creature"),
            &[],
        );
        assert_eq!(cat, GapCategory::NewMechanic);
    }

    #[test]
    fn classify_static_condition() {
        let (cat, _, _) = classify_gap(
            "Static:Unrecognized(some condition)",
            Some("as long as something"),
            &[],
        );
        assert_eq!(cat, GapCategory::StaticCondition);
    }

    #[test]
    fn classify_trigger_effect() {
        let (cat, _, _) = classify_gap(
            "Trigger:ChangesZone",
            Some("when this enters the battlefield"),
            &[
                "Trigger:ChangesZone".to_string(),
                "Effect:unknown".to_string(),
            ],
        );
        assert_eq!(cat, GapCategory::TriggerEffect);
    }

    #[test]
    fn classify_none_source_text() {
        let (cat, _, _) = classify_gap("Keyword:SomeKeyword", None, &[]);
        assert_eq!(cat, GapCategory::Unclassified);
    }

    #[test]
    fn near_miss_categories() {
        assert!(GapCategory::VerbVariation.is_near_miss());
        assert!(GapCategory::SubjectStripping.is_near_miss());
        assert!(GapCategory::TriggerEffect.is_near_miss());
        assert!(GapCategory::StaticCondition.is_near_miss());
        assert!(!GapCategory::NewMechanic.is_near_miss());
        assert!(!GapCategory::Unclassified.is_near_miss());
    }

    /// Verify key verbs in RECOGNIZED_VERBS are actually handled by the parser
    /// by parsing a canonical phrase and checking it doesn't return Unimplemented.
    #[test]
    fn recognized_verbs_parse_successfully() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::Effect;

        // Canonical test phrases for verbs — each should parse to a non-Unimplemented effect.
        // Not exhaustive (some verbs require card context), but covers the core set.
        let test_phrases: &[(&str, &str)] = &[
            ("destroy", "destroy target creature"),
            ("exile", "exile target creature"),
            ("draw", "draw a card"),
            ("discard", "discard a card"),
            ("sacrifice", "sacrifice a creature"),
            ("create", "create a 1/1 white Soldier creature token"),
            ("search", "search your library for a card"),
            ("scry", "scry 2"),
            ("surveil", "surveil 2"),
            ("mill", "mill 3"),
            ("tap", "tap target creature"),
            ("untap", "untap target creature"),
            ("return", "return target creature to its owner's hand"),
            ("counter", "counter target spell"),
            ("reveal", "reveal the top card of your library"),
            ("shuffle", "shuffle your library"),
            ("transform", "transform this creature"),
            ("gain", "gain 3 life"),
            ("lose", "lose 3 life"),
            ("put", "put a +1/+1 counter on target creature"),
            ("add", "add {G}"),
            ("explore", "explore"),
            ("proliferate", "proliferate"),
            ("investigate", "investigate"),
        ];

        for (verb, phrase) in test_phrases {
            let effect = parse_effect(phrase);
            assert!(
                !matches!(effect, Effect::Unimplemented { .. }),
                "Verb '{}' with phrase '{}' returned Unimplemented — parser doesn't handle it",
                verb,
                phrase
            );
        }
    }
}
