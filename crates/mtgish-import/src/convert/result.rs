//! Strict-failure discipline for the mtgish → engine converter.
//!
//! Per the plan (`§Partial-Translation Prevention`), no `Effect::Unimplemented`
//! fallback exists in this crate's normal output. Any sub-converter that cannot
//! produce a faithful engine form returns `Err(ConversionGap::*)`, which
//! propagates upward and drops the entire card from output. The gap is logged
//! to the import report so the work queue stays visible.

use std::fmt;

#[derive(Debug, Clone)]
pub enum ConversionGap {
    /// A serde-tagged variant we don't translate yet.
    /// `path` is a "/"-joined breadcrumb of nested enum variant names.
    UnknownVariant { path: String, repr: String },

    /// A recognized idiom shape was malformed (e.g., `MayCost` not paired
    /// with `If(<resolution-event>)` in the expected way).
    MalformedIdiom {
        idiom: &'static str,
        path: String,
        detail: String,
    },

    /// The engine type system lacks a primitive we need. The matching engine
    /// extension PR must land before this card can convert.
    EnginePrerequisiteMissing {
        engine_type: &'static str,
        needed_variant: String,
    },
}

impl ConversionGap {
    /// Stable string used as the report key. Identical gaps cluster.
    ///
    /// `UnknownVariant` includes the inner variant `repr` so distinct
    /// unknown variants under the same dispatch path get distinct keys
    /// (e.g. `"main/Rules[2]/Rule::TriggerA :: Trigger::WhenAPlayerCycles"`
    /// vs `"main/Rules[2]/Rule::TriggerA :: Trigger::FooBar"`). When the
    /// `path` is empty (sub-converter Err with no breadcrumb context),
    /// only the `repr` is used; the dispatcher's `enrich_gap_path`
    /// helper layers the rule context on top.
    pub fn report_path(&self) -> String {
        match self {
            ConversionGap::UnknownVariant { path, repr } => {
                if path.is_empty() {
                    repr.clone()
                } else if repr.is_empty() {
                    path.clone()
                } else {
                    format!("{path} :: {repr}")
                }
            }
            ConversionGap::MalformedIdiom {
                idiom,
                path,
                detail,
            } => {
                // Sub-bin by the leading colon-separated discriminator of
                // `detail` so high-fanin idiom buckets (notably
                // `GameNumber/convert`, with ~177 occurrences across many
                // distinct shapes) split into actionable sub-buckets in
                // the report. `detail` is free-form per-call-site text,
                // but by convention sub-converters lead with a stable
                // discriminator phrase ("non-You player", "unsupported
                // Players", etc.) followed by `: <AST debug>`. Truncating
                // at the first `:` strips the card-specific AST tail
                // while preserving the variant-class hint.
                let discriminator = detail
                    .split_once(':')
                    .map(|(d, _)| d.trim())
                    .unwrap_or(detail.trim());
                let suffix = if discriminator.is_empty() {
                    String::new()
                } else {
                    format!(" | {discriminator}")
                };
                if path.is_empty() {
                    format!("MalformedIdiom[{idiom}{suffix}]")
                } else {
                    format!("MalformedIdiom[{idiom}{suffix}]/{path}")
                }
            }
            ConversionGap::EnginePrerequisiteMissing {
                engine_type,
                needed_variant,
            } => format!("EnginePrerequisite/{engine_type}::{needed_variant}"),
        }
    }
}

impl fmt::Display for ConversionGap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConversionGap::UnknownVariant { path, repr } => {
                write!(f, "unknown variant at {path}: {repr}")
            }
            ConversionGap::MalformedIdiom {
                idiom,
                path,
                detail,
            } => write!(f, "malformed idiom {idiom} at {path}: {detail}"),
            ConversionGap::EnginePrerequisiteMissing {
                engine_type,
                needed_variant,
            } => write!(
                f,
                "engine type {engine_type} is missing variant {needed_variant}"
            ),
        }
    }
}

pub type ConvResult<T> = Result<T, ConversionGap>;
