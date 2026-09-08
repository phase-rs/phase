//! Deterministic, opt-in observation of the Oracle parser's document pipeline.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::doc::{OracleDocIr, OracleItemId, OracleSourceSpan};
use crate::parser::audit_projection::omit_definition_descriptions;
use crate::parser::oracle::ParsedAbilities;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadVisibility {
    NativeIr,
    AssembledOrPrelowered,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum OuterRoute {
    #[serde(rename = "preprocessor.class")]
    Class,
    #[serde(rename = "preprocessor.saga")]
    Saga,
    #[serde(rename = "preprocessor.attraction")]
    Attraction,
    #[serde(rename = "preprocessor.leveler")]
    Leveler,
    #[serde(rename = "preprocessor.spacecraft")]
    Spacecraft,
    #[serde(rename = "preprocessor.strive")]
    Strive,
    #[serde(rename = "router.keyword")]
    Keyword,
    #[serde(rename = "router.split_keyword")]
    SplitKeyword,
    #[serde(rename = "router.trigger")]
    Trigger,
    #[serde(rename = "router.activated")]
    Activated,
    #[serde(rename = "router.static")]
    Static,
    #[serde(rename = "router.replacement")]
    Replacement,
    #[serde(rename = "router.imperative_effect")]
    ImperativeEffect,
    #[serde(rename = "router.nom_dispatch")]
    NomDispatch,
    #[serde(rename = "router.unsupported")]
    Unsupported,
    #[serde(rename = "singleton.modal")]
    Modal,
    #[serde(rename = "singleton.additional_cost")]
    AdditionalCost,
    #[serde(rename = "singleton.casting_restriction")]
    CastingRestriction,
    #[serde(rename = "singleton.casting_option")]
    CastingOption,
    #[serde(rename = "singleton.solve")]
    Solve,
    #[serde(rename = "singleton.strive")]
    StriveSingleton,
}

impl OuterRoute {
    pub fn stable_id(self) -> &'static str {
        match self {
            Self::Class => "preprocessor.class",
            Self::Saga => "preprocessor.saga",
            Self::Attraction => "preprocessor.attraction",
            Self::Leveler => "preprocessor.leveler",
            Self::Spacecraft => "preprocessor.spacecraft",
            Self::Strive => "preprocessor.strive",
            Self::Keyword => "router.keyword",
            Self::SplitKeyword => "router.split_keyword",
            Self::Trigger => "router.trigger",
            Self::Activated => "router.activated",
            Self::Static => "router.static",
            Self::Replacement => "router.replacement",
            Self::ImperativeEffect => "router.imperative_effect",
            Self::NomDispatch => "router.nom_dispatch",
            Self::Unsupported => "router.unsupported",
            Self::Modal => "singleton.modal",
            Self::AdditionalCost => "singleton.additional_cost",
            Self::CastingRestriction => "singleton.casting_restriction",
            Self::CastingOption => "singleton.casting_option",
            Self::Solve => "singleton.solve",
            Self::StriveSingleton => "singleton.strive",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OuterRouteEvent {
    #[serde(skip)]
    pub(crate) item_id: OracleItemId,
    pub item: String,
    pub span: OracleSourceSpan,
    pub route: OuterRoute,
    pub payload_visibility: PayloadVisibility,
}

#[derive(Default)]
pub(crate) struct ParseObserver {
    pub(crate) events: Vec<OuterRouteEvent>,
}

impl ParseObserver {
    pub(crate) fn record(
        &mut self,
        item_id: OracleItemId,
        span: OracleSourceSpan,
        route: OuterRoute,
        payload_visibility: PayloadVisibility,
    ) {
        self.events.push(OuterRouteEvent {
            item_id,
            item: String::new(),
            span,
            route,
            payload_visibility,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmittedEvidence {
    pub stage: TraceStage,
    pub path: String,
    pub carrier: DefinitionCarrier,
    pub value: String,
    #[serde(skip_serializing_if = "is_false")]
    pub identity_sensitive_omission: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionCarrier {
    AbilityDefinition,
    TriggerDefinition,
    StaticDefinition,
    ReplacementDefinition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceStage {
    OriginalSource,
    NormalizedSource,
    DocumentIr,
    RawLoweredIr,
    ProductionParsedOutput,
}

#[derive(Debug, Clone)]
pub struct ParserTrace {
    pub original_source: String,
    pub normalized_source: String,
    pub document_ir_candidate: Value,
    pub raw_lowered_candidate: Value,
    pub production_candidate: Value,
    pub events: Vec<OuterRouteEvent>,
    pub omitted_evidence: Vec<OmittedEvidence>,
    pub production_output: ParsedAbilities,
}

pub(crate) fn document_candidate(doc: &OracleDocIr, evidence: &mut Vec<OmittedEvidence>) -> Value {
    let mut value = serde_json::to_value(doc).expect("OracleDocIr serialization is infallible");
    if let Value::Object(root) = &mut value {
        root.remove("source_text");
        root.remove("card_name");
        if let Some(Value::Array(items)) = root.get_mut("items") {
            for item in items.iter_mut() {
                if let Value::Object(item) = item {
                    item.remove("source");
                }
            }
        }
    }
    append_shared_omissions(&mut value, TraceStage::DocumentIr, evidence);
    value
}

pub(crate) fn parsed_candidate(
    parsed: &ParsedAbilities,
    stage: TraceStage,
    evidence: &mut Vec<OmittedEvidence>,
) -> Value {
    let mut value =
        serde_json::to_value(parsed).expect("ParsedAbilities serialization is infallible");
    append_shared_omissions(&mut value, stage, evidence);
    value
}

fn append_shared_omissions(
    value: &mut Value,
    stage: TraceStage,
    evidence: &mut Vec<OmittedEvidence>,
) {
    evidence.extend(
        omit_definition_descriptions(value)
            .into_iter()
            .map(|omission| {
                let carrier = match omission.carrier {
                    // allow-noncombinator: closed report-schema label, not Oracle text dispatch.
                    "AbilityDefinition" => DefinitionCarrier::AbilityDefinition,
                    // allow-noncombinator: closed report-schema label, not Oracle text dispatch.
                    "TriggerDefinition" => DefinitionCarrier::TriggerDefinition,
                    // allow-noncombinator: closed report-schema label, not Oracle text dispatch.
                    "StaticDefinition" => DefinitionCarrier::StaticDefinition,
                    // allow-noncombinator: closed report-schema label, not Oracle text dispatch.
                    "ReplacementDefinition" => DefinitionCarrier::ReplacementDefinition,
                    _ => unreachable!("schema projector has a closed carrier vocabulary"),
                };
                OmittedEvidence {
                    stage,
                    path: omission.json_pointer,
                    value: omission.value,
                    carrier,
                    identity_sensitive_omission: carrier == DefinitionCarrier::TriggerDefinition,
                }
            }),
    );
}

pub fn sha256_json(value: &Value) -> String {
    sha256_bytes(&serde_json::to_vec(value).expect("JSON value serialization is infallible"))
}
pub fn sha256_string(value: &str) -> String {
    sha256_bytes(&serde_json::to_vec(value).expect("string serialization is infallible"))
}
pub fn sha256_bytes(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairManifest {
    pub schema_version: u32,
    pub pairs: Vec<PairManifestEntry>,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairManifestEntry {
    pub left_card_face_key: String,
    pub right_card_face_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FaceRef {
    pub card_face_key: String,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StageComparison {
    pub stage: TraceStage,
    pub left_sha256: String,
    pub right_sha256: String,
    pub candidate_equal: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Collapse {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<TraceStage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<TraceStage>,
    pub non_monotonic: bool,
}

pub fn classify_collapse(stages: &[StageComparison]) -> Collapse {
    if stages.first().is_some_and(|s| s.candidate_equal) {
        return Collapse {
            kind: "already_equal_at_input".into(),
            from: None,
            to: None,
            non_monotonic: stages.iter().any(|s| !s.candidate_equal),
        };
    }
    let first = stages
        .windows(2)
        .find(|w| !w[0].candidate_equal && w[1].candidate_equal);
    let (kind, from, to) = first.map_or(("no_collapse", None, None), |w| {
        ("first_false_to_true", Some(w[0].stage), Some(w[1].stage))
    });
    let non_monotonic = first.is_some_and(|w| {
        let start = stages.iter().position(|s| s.stage == w[1].stage).unwrap();
        stages[start + 1..].iter().any(|s| !s.candidate_equal)
    });
    Collapse {
        kind: kind.into(),
        from,
        to,
        non_monotonic,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairReport {
    pub left: FaceRef,
    pub right: FaceRef,
    pub stages: Vec<StageComparison>,
    pub collapse: Collapse,
    pub left_outer_route_events: Vec<OuterRouteEvent>,
    pub right_outer_route_events: Vec<OuterRouteEvent>,
    pub shared_outer_routes: Vec<OuterRoute>,
    pub location_precision: &'static str,
    pub limitation_codes: BTreeSet<String>,
    pub omitted_evidence: PairOmittedEvidence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairOmittedEvidence {
    pub left: Vec<OmittedEvidence>,
    pub right: Vec<OmittedEvidence>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CensusFace {
    pub card_face_key: String,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OuterRouteCensusRow {
    pub route: OuterRoute,
    pub event_count: usize,
    pub face_count: usize,
    pub card_faces: Vec<CensusFace>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FaceCounts {
    pub winning_faces: usize,
    pub traced_faces: usize,
    pub unavailable_events: usize,
    pub faces_with_unavailable_events: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParserTraceReport {
    pub schema_version: u32,
    pub algorithm_version: &'static str,
    pub card_data_sha256: String,
    pub faces: FaceCounts,
    pub pairs: Vec<PairReport>,
    pub outer_route_census: Vec<OuterRouteCensusRow>,
}

pub fn canonicalize_events(side: &str, events: &mut [OuterRouteEvent]) {
    let mut ids = BTreeMap::new();
    for event in events.iter() {
        let next = ids.len();
        ids.entry(event.item_id).or_insert(next);
    }
    for event in events.iter_mut() {
        event.item = format!("{side}/item:{}", ids[&event.item_id]);
    }
    events.sort_by(|a, b| {
        a.item
            .cmp(&b.item)
            .then(a.route.stable_id().cmp(b.route.stable_id()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_trace_collapse_classification_is_non_monotonic_aware() {
        let mk = |stage, equal| StageComparison {
            stage,
            left_sha256: String::new(),
            right_sha256: String::new(),
            candidate_equal: equal,
        };
        let stages = vec![
            mk(TraceStage::OriginalSource, false),
            mk(TraceStage::NormalizedSource, true),
            mk(TraceStage::DocumentIr, false),
        ];
        let collapse = classify_collapse(&stages);
        assert_eq!(collapse.kind, "first_false_to_true");
        assert!(collapse.non_monotonic);
    }
}
