//! The `parser-gap-analyzer` report. It regroups the coverage summary's gaps by their
//! typed diagnosis (the category) and by the phrase, feature or handler that diagnosis
//! names (the family). For each category and family, it reports how many cards the gaps
//! affect and how many cards that fix alone would make supported.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use strum::IntoEnumIterator;

use crate::game::coverage::{
    normalize_oracle_pattern, CardCoverageResult, GapDetail, GapDiagnosis, ResolverFeatureFamily,
};
use crate::parser::oracle_ir::diagnostic::{ClauseGap, ClauseGapKind};

/// A report category: the layer that explains a gap, refined by the parser verdict or
/// the resolver feature family. A gap with no diagnosis is `Undiagnosed`; its family is
/// its coverage handler, and the report never derives a verdict from the gap's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapClass {
    Parser(ClauseGapKind),
    Resolver(ResolverFeatureFamily),
    Undiagnosed,
}

impl GapClass {
    /// Every category, in declaration order.
    pub fn all() -> impl Iterator<Item = Self> {
        ClauseGapKind::iter()
            .map(Self::Parser)
            .chain(ResolverFeatureFamily::iter().map(Self::Resolver))
            .chain(std::iter::once(Self::Undiagnosed))
    }

    /// The category's key in the report, and the value `--category` accepts. Each half
    /// is the diagnosis's own wire tag, so a key reads the same as the `kind` or
    /// `family` field in the coverage export.
    pub fn label(self) -> String {
        match self {
            Self::Parser(kind) => format!("parser:{}", kind.unimplemented_name()),
            Self::Resolver(family) => format!("resolver:{}", family.tag()),
            Self::Undiagnosed => "undiagnosed".to_string(),
        }
    }

    /// Decodes a report key back to its category, or `None` for an unknown key.
    pub fn from_label(label: &str) -> Option<Self> {
        Self::all().find(|class| class.label() == label)
    }
}

/// Counts and card lists for one category or family. `cards_affected` and `fixes_alone`
/// are the lengths of the lists beside them; `count` counts gaps, not cards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GapTally {
    /// Gaps in this category or family. A gap with several diagnoses here counts once.
    pub count: usize,
    pub cards_affected: usize,
    /// Cards whose every gap is in this category or family and in no other.
    pub fixes_alone: usize,
    pub fixes_alone_cards: Vec<String>,
    pub affected_cards: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FamilySummary {
    /// The rejected phrase under coverage's pattern normalizer (parser; prefixed
    /// `"<verb>: "` for verb arguments), the feature name (resolver), or the coverage
    /// handler (undiagnosed).
    pub key: String,
    #[serde(flatten)]
    pub tally: GapTally,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategorySummary {
    #[serde(flatten)]
    pub tally: GapTally,
    /// Ordered by `fixes_alone` and then `cards_affected`, both descending, then by `key`.
    pub families: Vec<FamilySummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GapAnalysis {
    pub analysis_date: String,
    /// Distinct card names among unsupported cards.
    pub total_unsupported: usize,
    /// Gaps on unsupported cards.
    pub total_classified: usize,
    /// Keyed by [`GapClass::label`].
    pub categories: BTreeMap<String, CategorySummary>,
}

/// One (category, family) pair that a gap belongs to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GapKey {
    category: String,
    family: String,
}

/// The report keys of one gap: one per distinct diagnosis, or its handler under
/// `Undiagnosed` when it carries none.
fn gap_keys(gap: &GapDetail) -> BTreeSet<GapKey> {
    if gap.diagnoses.is_empty() {
        return BTreeSet::from([GapKey {
            category: GapClass::Undiagnosed.label(),
            family: gap.handler.clone(),
        }]);
    }
    gap.diagnoses
        .iter()
        .map(|diagnosis| match diagnosis {
            GapDiagnosis::Parser(clause) => GapKey {
                category: GapClass::Parser(clause.kind()).label(),
                family: parser_family(clause),
            },
            GapDiagnosis::Resolver { family, feature } => GapKey {
                category: GapClass::Resolver(*family).label(),
                family: feature.clone(),
            },
        })
        .collect()
}

/// A parser verdict's family: the phrase it rejected, under the normalizer coverage's
/// `oracle_patterns` uses. For verb arguments the verb is part of the family, because
/// that verb's argument grammar did the rejecting.
fn parser_family(clause: &ClauseGap) -> String {
    let phrase = normalize_oracle_pattern(clause.phrase());
    match clause {
        ClauseGap::VerbArguments { verb, .. } => format!("{verb}: {phrase}"),
        ClauseGap::Replacement { .. }
        | ClauseGap::Condition { .. }
        | ClauseGap::Quantity { .. }
        | ClauseGap::UnrecognizedHead { .. } => phrase,
    }
}

#[derive(Default)]
struct Tally<'a> {
    count: usize,
    affected: BTreeSet<&'a str>,
    fixes_alone: BTreeSet<&'a str>,
}

impl<'a> Tally<'a> {
    /// Records a card that has a gap here.
    fn affect(&mut self, card: &'a str) {
        self.affected.insert(card);
    }

    /// Records a card whose every gap is here. Such a card is affected too.
    fn fix_alone(&mut self, card: &'a str) {
        self.affect(card);
        self.fixes_alone.insert(card);
    }

    /// Builds both card lists in one place, so they share one order (by name).
    fn finish(self) -> GapTally {
        let owned =
            |cards: BTreeSet<&str>| cards.into_iter().map(str::to_string).collect::<Vec<_>>();
        GapTally {
            count: self.count,
            cards_affected: self.affected.len(),
            fixes_alone: self.fixes_alone.len(),
            fixes_alone_cards: owned(self.fixes_alone),
            affected_cards: owned(self.affected),
        }
    }
}

#[derive(Default)]
struct CategoryTally<'a> {
    tally: Tally<'a>,
    families: BTreeMap<String, Tally<'a>>,
}

impl CategoryTally<'_> {
    fn finish(self) -> CategorySummary {
        let mut families: Vec<FamilySummary> = self
            .families
            .into_iter()
            .map(|(key, tally)| FamilySummary {
                key,
                tally: tally.finish(),
            })
            .collect();
        families.sort_by(|a, b| {
            b.tally
                .fixes_alone
                .cmp(&a.tally.fixes_alone)
                .then(b.tally.cards_affected.cmp(&a.tally.cards_affected))
                .then_with(|| a.key.cmp(&b.key))
        });
        CategorySummary {
            tally: self.tally.finish(),
            families,
        }
    }
}

/// Groups every gap on every unsupported card by category and family.
pub fn analyze_gaps(cards: &[CardCoverageResult]) -> GapAnalysis {
    let mut categories: BTreeMap<String, CategoryTally> = BTreeMap::new();
    // A card is its name. Results that share a printed name pool their gaps, so a family
    // fixes a card alone only if every gap under that name is in the family.
    let mut keys_by_card: BTreeMap<&str, BTreeSet<GapKey>> = BTreeMap::new();
    let mut total_classified = 0;

    for card in cards.iter().filter(|card| !card.supported) {
        let card_keys = keys_by_card.entry(card.card_name.as_str()).or_default();
        for gap in &card.gap_details {
            total_classified += 1;
            let keys = gap_keys(gap);
            let gap_categories: BTreeSet<&str> =
                keys.iter().map(|key| key.category.as_str()).collect();
            for category in gap_categories {
                categories
                    .entry(category.to_string())
                    .or_default()
                    .tally
                    .count += 1;
            }
            for key in &keys {
                categories
                    .entry(key.category.clone())
                    .or_default()
                    .families
                    .entry(key.family.clone())
                    .or_default()
                    .count += 1;
            }
            card_keys.extend(keys);
        }
    }

    for (&card, keys) in &keys_by_card {
        let card_categories: BTreeSet<&str> =
            keys.iter().map(|key| key.category.as_str()).collect();
        for key in keys {
            let family = categories
                .entry(key.category.clone())
                .or_default()
                .families
                .entry(key.family.clone())
                .or_default();
            if keys.len() == 1 {
                family.fix_alone(card);
            } else {
                family.affect(card);
            }
        }
        for category in &card_categories {
            let tally = &mut categories.entry(category.to_string()).or_default().tally;
            if card_categories.len() == 1 {
                tally.fix_alone(card);
            } else {
                tally.affect(card);
            }
        }
    }

    GapAnalysis {
        analysis_date: String::new(),
        total_unsupported: keys_by_card.len(),
        total_classified,
        categories: categories
            .into_iter()
            .map(|(label, category)| (label, category.finish()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_effect::parse_effect;
    use crate::types::ability::Effect;

    fn unsupported(name: &str, gap_details: Vec<GapDetail>) -> CardCoverageResult {
        CardCoverageResult {
            card_face_key: None,
            card_name: name.to_string(),
            set_code: String::new(),
            supported: false,
            gap_count: gap_details.len(),
            gap_details,
            oracle_text: None,
            parse_details: Vec::new(),
            printings: Vec::new(),
        }
    }

    fn gap(handler: &str, diagnoses: Vec<GapDiagnosis>) -> GapDetail {
        GapDetail {
            handler: handler.to_string(),
            source_text: None,
            diagnoses,
        }
    }

    fn condition(guard: &str) -> GapDiagnosis {
        GapDiagnosis::Parser(ClauseGap::Condition {
            guard: guard.to_string(),
        })
    }

    fn verb(verb: &str, arguments: &str) -> GapDiagnosis {
        GapDiagnosis::Parser(ClauseGap::VerbArguments {
            verb: verb.to_string(),
            arguments: arguments.to_string(),
        })
    }

    fn feature(family: ResolverFeatureFamily, name: &str) -> GapDiagnosis {
        GapDiagnosis::Resolver {
            family,
            feature: name.to_string(),
        }
    }

    fn category(analysis: &GapAnalysis, class: GapClass) -> &CategorySummary {
        analysis
            .categories
            .get(&class.label())
            .unwrap_or_else(|| panic!("no category {}", class.label()))
    }

    fn family<'a>(summary: &'a CategorySummary, key: &str) -> &'a GapTally {
        &summary
            .families
            .iter()
            .find(|family| family.key == key)
            .unwrap_or_else(|| panic!("no family {key:?}"))
            .tally
    }

    fn family_keys(summary: &CategorySummary) -> Vec<&str> {
        summary
            .families
            .iter()
            .map(|family| family.key.as_str())
            .collect()
    }

    fn keys(analysis: &GapAnalysis) -> Vec<String> {
        analysis.categories.keys().cloned().collect()
    }

    /// The expected tally. `cards_affected` and `fixes_alone` are taken from their lists'
    /// lengths, so each card count and its list are asserted together.
    fn tally(count: usize, affected: &[&str], alone: &[&str]) -> GapTally {
        let owned = |cards: &[&str]| {
            cards
                .iter()
                .map(|card| card.to_string())
                .collect::<Vec<_>>()
        };
        GapTally {
            count,
            cards_affected: affected.len(),
            fixes_alone: alone.len(),
            fixes_alone_cards: owned(alone),
            affected_cards: owned(affected),
        }
    }

    #[test]
    fn gap_class_labels_are_distinct_and_round_trip() {
        let labels: Vec<String> = GapClass::all().map(GapClass::label).collect();
        assert_eq!(
            labels.len(),
            ClauseGapKind::iter().count() + ResolverFeatureFamily::iter().count() + 1
        );
        assert_eq!(
            labels.iter().collect::<BTreeSet<_>>().len(),
            labels.len(),
            "{labels:?}"
        );
        for class in GapClass::all() {
            assert_eq!(GapClass::from_label(&class.label()), Some(class));
        }
        for unknown in [
            "",
            "parser:",
            "resolver:",
            "parser:unknown",
            "Undiagnosed",
            "PARSER:UNPARSED_CONDITION",
            "A",
        ] {
            assert_eq!(
                GapClass::from_label(unknown),
                None,
                "{unknown:?} must not decode"
            );
        }
        // The key halves are the coverage export's wire tags, which the report gate reads.
        for family in ResolverFeatureFamily::iter() {
            assert_eq!(
                serde_json::to_value(family).unwrap(),
                serde_json::json!(family.tag())
            );
        }
    }

    #[test]
    fn a_family_fixes_alone_only_the_cards_whose_every_gap_is_in_it() {
        let quantity = GapDiagnosis::Parser(ClauseGap::Quantity {
            operand: "the number of elves".to_string(),
        });
        let analysis = analyze_gaps(&[
            unsupported(
                "Alpha",
                vec![gap(
                    "Effect:unparsed_condition",
                    vec![condition("if you control an elf")],
                )],
            ),
            // Two gaps that normalize to one family, so the family still fixes Beta alone.
            unsupported(
                "Beta",
                vec![
                    gap(
                        "Effect:unparsed_condition",
                        vec![condition("If you control an Elf.")],
                    ),
                    gap(
                        "Swallow:Condition_If",
                        vec![condition("if you control an elf")],
                    ),
                ],
            ),
            // One gap in the family and one outside it: affected, not fixed alone.
            unsupported(
                "Gamma",
                vec![
                    gap(
                        "Effect:unparsed_condition",
                        vec![condition("if you control an elf")],
                    ),
                    gap("Effect:unparsed_quantity", vec![quantity]),
                ],
            ),
        ]);
        let conditions = category(&analysis, GapClass::Parser(ClauseGapKind::Condition));
        let expected = tally(4, &["Alpha", "Beta", "Gamma"], &["Alpha", "Beta"]);
        assert_eq!(*family(conditions, "if you control an elf"), expected);
        assert_eq!(conditions.tally, expected);
        assert_eq!(
            category(&analysis, GapClass::Parser(ClauseGapKind::Quantity)).tally,
            tally(1, &["Gamma"], &[])
        );
        assert_eq!(
            (analysis.total_unsupported, analysis.total_classified),
            (3, 5)
        );
    }

    /// C4.3b. The two summaries differ in one variable only: the layer of Delta's second
    /// diagnosis. The handler, the card and the first gap are identical.
    #[test]
    fn a_resolver_gap_is_its_own_layer_and_never_inflates_a_parser_family() {
        let summary = |second: GapDiagnosis| {
            analyze_gaps(&[unsupported(
                "Delta",
                vec![
                    gap(
                        "Effect:unparsed_condition",
                        vec![condition("if it's your turn")],
                    ),
                    gap("Effect:second", vec![second]),
                ],
            )])
        };
        let parser_only = summary(condition("if it's your turn"));
        let with_resolver = summary(feature(ResolverFeatureFamily::StaticCondition, "Future"));

        let parser = GapClass::Parser(ClauseGapKind::Condition);
        let resolver = GapClass::Resolver(ResolverFeatureFamily::StaticCondition);
        assert_eq!(keys(&parser_only), vec![parser.label()]);
        assert_eq!(keys(&with_resolver), vec![parser.label(), resolver.label()]);
        assert_eq!(
            *family(category(&parser_only, parser), "if it's your turn"),
            tally(2, &["Delta"], &["Delta"])
        );
        assert_eq!(
            *family(category(&with_resolver, parser), "if it's your turn"),
            tally(1, &["Delta"], &[])
        );
        assert_eq!(
            *family(category(&with_resolver, resolver), "Future"),
            tally(1, &["Delta"], &[])
        );
    }

    /// C4.4. A gap without a diagnosis is reported under its handler, and its text is
    /// never read, even when the text starts with a clause-head verb.
    #[test]
    fn undiagnosed_gaps_are_keyed_by_handler_not_by_text() {
        let with_text = |handler: &str, text: &str| GapDetail {
            handler: handler.to_string(),
            source_text: Some(text.to_string()),
            diagnoses: Vec::new(),
        };
        let analysis = analyze_gaps(&[
            unsupported(
                "Epsilon",
                vec![with_text("Effect:unknown", "destroy target creature")],
            ),
            unsupported(
                "Eta",
                vec![with_text("Effect:unknown", "exile target creature")],
            ),
            unsupported(
                "Zeta",
                vec![
                    gap("Static:Unrecognized(as long as it's night)", vec![]),
                    gap("Effect:empty", vec![]),
                ],
            ),
        ]);
        assert_eq!(keys(&analysis), vec![GapClass::Undiagnosed.label()]);
        let undiagnosed = category(&analysis, GapClass::Undiagnosed);
        assert_eq!(
            family_keys(undiagnosed),
            [
                "Effect:unknown",
                "Effect:empty",
                "Static:Unrecognized(as long as it's night)"
            ]
        );
        assert_eq!(
            *family(undiagnosed, "Effect:unknown"),
            tally(2, &["Epsilon", "Eta"], &["Epsilon", "Eta"])
        );
        // Zeta's gaps fall in two families, so neither family fixes it alone, but the category does.
        assert_eq!(
            *family(undiagnosed, "Effect:empty"),
            tally(1, &["Zeta"], &[])
        );
        assert_eq!(
            undiagnosed.tally,
            tally(4, &["Epsilon", "Eta", "Zeta"], &["Epsilon", "Eta", "Zeta"])
        );
    }

    #[test]
    fn a_gap_with_several_verdicts_counts_once_per_family_and_once_per_category() {
        let analysis = analyze_gaps(&[
            // The coverage merge keeps a repeated verdict; the report counts it once.
            unsupported(
                "Theta",
                vec![gap(
                    "Swallow:Condition_If",
                    vec![condition("if a"), condition("if b"), condition("if a")],
                )],
            ),
            unsupported(
                "Iota",
                vec![gap(
                    "Effect:unparsed_verb_arguments",
                    vec![verb("tap", "up to two lands")],
                )],
            ),
            unsupported(
                "Kappa",
                vec![gap(
                    "Effect:unparsed_verb_arguments",
                    vec![verb("untap", "up to two lands")],
                )],
            ),
        ]);
        let conditions = category(&analysis, GapClass::Parser(ClauseGapKind::Condition));
        assert_eq!(conditions.tally, tally(1, &["Theta"], &["Theta"]));
        assert_eq!(*family(conditions, "if a"), tally(1, &["Theta"], &[]));
        assert_eq!(*family(conditions, "if b"), tally(1, &["Theta"], &[]));
        // One argument phrase under two verbs makes two families.
        let verbs = category(&analysis, GapClass::Parser(ClauseGapKind::VerbArguments));
        assert_eq!(
            *family(verbs, "tap: up to two lands"),
            tally(1, &["Iota"], &["Iota"])
        );
        assert_eq!(
            *family(verbs, "untap: up to two lands"),
            tally(1, &["Kappa"], &["Kappa"])
        );
    }

    #[test]
    fn families_rank_by_fixes_alone_then_reach_then_key_and_card_lists_by_name() {
        let alone = |name: &str, guard: &str| {
            unsupported(
                name,
                vec![gap("Effect:unparsed_condition", vec![condition(guard)])],
            )
        };
        let analysis = analyze_gaps(&[
            alone("Zulu", "if b"),
            alone("Alpha", "if b"),
            alone("November", "if d"),
            alone("Mike", "if c"),
            alone("Lima", "if e"),
            unsupported(
                "Echo",
                vec![
                    gap("Effect:unparsed_condition", vec![condition("if e")]),
                    gap("Effect:unknown", vec![]),
                ],
            ),
            unsupported(
                "Foxtrot",
                vec![
                    gap("Effect:unparsed_condition", vec![condition("if e")]),
                    gap("Effect:unknown", vec![]),
                ],
            ),
        ]);
        let conditions = category(&analysis, GapClass::Parser(ClauseGapKind::Condition));
        assert_eq!(family_keys(conditions), ["if b", "if e", "if c", "if d"]);
        assert_eq!(
            *family(conditions, "if b"),
            tally(2, &["Alpha", "Zulu"], &["Alpha", "Zulu"])
        );
        assert_eq!(
            *family(conditions, "if e"),
            tally(3, &["Echo", "Foxtrot", "Lima"], &["Lima"])
        );
    }

    #[test]
    fn the_report_serializes_every_tally_key_even_when_empty() {
        let analysis = analyze_gaps(&[unsupported(
            "Lambda",
            vec![
                gap("Effect:unparsed_condition", vec![condition("if x")]),
                gap("Effect:unknown", vec![]),
            ],
        )]);
        let json = serde_json::to_value(&analysis).unwrap();
        fn key_set(value: &serde_json::Value) -> BTreeSet<&str> {
            value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect()
        }
        assert_eq!(
            key_set(&json),
            BTreeSet::from([
                "analysis_date",
                "categories",
                "total_classified",
                "total_unsupported"
            ])
        );
        let undiagnosed = &json["categories"]["undiagnosed"];
        assert_eq!(
            key_set(undiagnosed),
            BTreeSet::from([
                "affected_cards",
                "cards_affected",
                "count",
                "families",
                "fixes_alone",
                "fixes_alone_cards"
            ])
        );
        assert_eq!(
            key_set(&undiagnosed["families"][0]),
            BTreeSet::from([
                "affected_cards",
                "cards_affected",
                "count",
                "fixes_alone",
                "fixes_alone_cards",
                "key"
            ])
        );
        assert_eq!(undiagnosed["fixes_alone_cards"], serde_json::json!([]));
    }

    #[test]
    fn supported_cards_and_empty_input_contribute_nothing() {
        let with_gap = || unsupported("Mu", vec![gap("Effect:unknown", vec![])]);
        // Reach guard: the same card, unsupported, does contribute.
        assert_eq!(
            keys(&analyze_gaps(&[with_gap()])),
            vec![GapClass::Undiagnosed.label()]
        );
        let mut supported = with_gap();
        supported.supported = true;
        for cards in [vec![], vec![supported]] {
            let analysis = analyze_gaps(&cards);
            assert!(analysis.categories.is_empty());
            assert_eq!(
                (analysis.total_unsupported, analysis.total_classified),
                (0, 0)
            );
        }
    }

    #[test]
    fn results_sharing_a_name_pool_their_gaps() {
        let analysis = analyze_gaps(&[
            unsupported(
                "Nu",
                vec![gap("Effect:unparsed_condition", vec![condition("if x")])],
            ),
            unsupported("Nu", vec![gap("Effect:unknown", vec![])]),
        ]);
        assert_eq!(analysis.total_unsupported, 1);
        let conditions = category(&analysis, GapClass::Parser(ClauseGapKind::Condition));
        assert_eq!(*family(conditions, "if x"), tally(1, &["Nu"], &[]));
        assert_eq!(
            category(&analysis, GapClass::Undiagnosed).tally,
            tally(1, &["Nu"], &[])
        );
    }

    /// B-2. Parser families key on the normalizer coverage's `oracle_patterns` uses, so
    /// phrases that differ only in a number are one family; a phrase that differs in a
    /// word is another.
    #[test]
    fn parser_families_key_on_the_coverage_pattern_normalizer() {
        let analysis = analyze_gaps(&[
            unsupported(
                "Omicron",
                vec![gap(
                    "Effect:unparsed_condition",
                    vec![condition("if you have 3 or more cards in hand")],
                )],
            ),
            unsupported(
                "Pi",
                vec![gap(
                    "Effect:unparsed_condition",
                    vec![condition("If you have 7 or more cards in hand.")],
                )],
            ),
            unsupported(
                "Rho",
                vec![gap(
                    "Effect:unparsed_condition",
                    vec![condition("if you have 3 or more lands in play")],
                )],
            ),
            unsupported(
                "Sigma",
                vec![gap(
                    "Effect:unparsed_verb_arguments",
                    vec![verb("deal", "3 damage to each opponent")],
                )],
            ),
            unsupported(
                "Tau",
                vec![gap(
                    "Effect:unparsed_verb_arguments",
                    vec![verb("deal", "5 damage to each opponent")],
                )],
            ),
        ]);
        let hand = normalize_oracle_pattern("if you have 3 or more cards in hand");
        assert_eq!(hand, "if you have N or more cards in hand");
        let conditions = category(&analysis, GapClass::Parser(ClauseGapKind::Condition));
        let lands = normalize_oracle_pattern("if you have 3 or more lands in play");
        assert_eq!(family_keys(conditions), [hand.as_str(), lands.as_str()]);
        assert_eq!(
            *family(conditions, &hand),
            tally(2, &["Omicron", "Pi"], &["Omicron", "Pi"])
        );
        let verbs = category(&analysis, GapClass::Parser(ClauseGapKind::VerbArguments));
        let deal = format!(
            "deal: {}",
            normalize_oracle_pattern("3 damage to each opponent")
        );
        assert_eq!(family_keys(verbs), [deal.as_str()]);
        assert_eq!(
            *family(verbs, &deal),
            tally(2, &["Sigma", "Tau"], &["Sigma", "Tau"])
        );
    }

    /// Verify core verbs are handled by the parser
    /// by parsing a canonical phrase and checking it doesn't return Unimplemented.
    #[test]
    fn recognized_verbs_parse_successfully() {
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
