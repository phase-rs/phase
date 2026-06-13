#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/new-ai-policy.sh <feature_name>

Creates phase-ai scaffold files:
  crates/phase-ai/src/features/<feature_name>.rs
  crates/phase-ai/src/policies/<feature_name>.rs
  crates/phase-ai/src/policies/mulligan/<feature_name>_keepables.rs

The script does not edit registry/module wiring. It prints the required
checklist after writing the files.
USAGE
  exit 2
}

if (($# != 1)); then
  usage
fi

snake="$1"
if [[ ! "$snake" =~ ^[a-z][a-z0-9_]*$ ]]; then
  echo "new-ai-policy: feature_name must be snake_case" >&2
  exit 2
fi

camel=$(awk -F_ '{ for (i = 1; i <= NF; i++) printf toupper(substr($i, 1, 1)) substr($i, 2) }' <<<"$snake")
root="crates/phase-ai/src"
feature_file="$root/features/$snake.rs"
policy_file="$root/policies/$snake.rs"
mulligan_file="$root/policies/mulligan/${snake}_keepables.rs"

for path in "$feature_file" "$policy_file" "$mulligan_file"; do
  if [[ -e "$path" ]]; then
    echo "new-ai-policy: refusing to overwrite existing file: $path" >&2
    exit 1
  fi
done

cat >"$feature_file" <<RS
//! ${camel} feature — structural detection over a deck's typed AST.
//!
//! Fill in the AST verification table before wiring this module. Detection
//! must inspect typed \`CardFace\` structures; never classify by card name.

use engine::game::DeckEntry;

/// Structural feature data for ${snake}.
#[derive(Debug, Clone, Default)]
pub struct ${camel}Feature {
    /// Density-normalized commitment in \`0.0..=1.0\`.
    pub commitment: f32,
    /// Identity lookup names for cards already classified structurally.
    pub payoff_names: Vec<String>,
}

/// Detect ${snake} support from deck entries.
///
/// Commitment formulas must use \`crate::features::commitment::weighted_sum\`
/// or \`crate::features::commitment::geometric_mean\` over per-60-nonland
/// densities from \`commitment::density_per_60\`.
pub fn detect(deck: &[DeckEntry]) -> ${camel}Feature {
    let _ = deck;
    // TODO: replace this neutral scaffold with structural detection and a
    // calibration anchor test.
    ${camel}Feature::default()
}
RS

cat >"$policy_file" <<RS
//! ${camel} tactical policy.
//!
//! Score contract:
//! - delta 1.0 = one card of expected value
//! - use \`PolicyVerdict::{nudge,preference,strong,critical}\`
//! - all scoring constants come from \`AiConfig::penalties\`
//! - hard vetoes use \`PolicyVerdict::reject\`, never sentinel scores

use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::features::DeckFeatures;
use crate::policies::context::PolicyContext;
use crate::policies::registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy,
};

pub struct ${camel}Policy;

impl TacticalPolicy for ${camel}Policy {
    fn id(&self) -> PolicyId {
        PolicyId::${camel}
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        (features.${snake}.commitment > 0.0).then_some(features.${snake}.commitment)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let reason = PolicyReason::new("${snake}_todo");
        // TODO(tune): add \`PolicyPenalties::${snake}_preference_bonus\` with a
        // rationale comment, then route this delta through that config field.
        PolicyVerdict::preference(ctx.config.penalties.${snake}_preference_bonus, reason)
    }
}
RS

cat >"$mulligan_file" <<RS
//! ${camel} mulligan policy.

use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::mulligan::{MulliganPolicy, MulliganScore, TurnOrder};
use crate::policies::registry::{PolicyId, PolicyReason};

pub struct ${camel}KeepablesMulligan;

impl MulliganPolicy for ${camel}KeepablesMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::${camel}KeepablesMulligan
    }

    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        plan: &PlanSnapshot,
        turn_order: TurnOrder,
        mulligans_taken: u8,
    ) -> MulliganScore {
        let _ = (hand, state, plan);
        // input-unused: remove this marker once turn_order changes scoring.
        let _ = turn_order;
        // input-unused: remove this marker once mulligans_taken changes scoring.
        let _ = mulligans_taken;

        if features.${snake}.commitment == 0.0 {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("${snake}_inactive"),
            };
        }

        // TODO(tune): route through a named mulligan config constant once one
        // exists; mulligan policies do not use TacticalPolicy band helpers.
        MulliganScore::Score {
            delta: 0.0,
            reason: PolicyReason::new("${snake}_todo"),
        }
    }
}
RS

cat <<CHECKLIST
Created:
  $feature_file
  $policy_file
  $mulligan_file

Required wiring checklist:
  1. Add \`pub mod $snake;\` and \`pub use ${snake}::${camel}Feature;\` in \`crates/phase-ai/src/features/mod.rs\`.
  2. Add \`${snake}: ${camel}Feature\` to \`DeckFeatures\` and call \`${snake}::detect(deck)\` in \`DeckFeatures::analyze\`.
  3. Add \`mod $snake;\` in \`crates/phase-ai/src/policies/mod.rs\`.
  4. Add \`PolicyId::${camel}\` and \`PolicyId::${camel}KeepablesMulligan\` in \`policies/registry.rs\`.
  5. Register \`${camel}Policy\` in \`PolicyRegistry::default()\`.
  6. Add \`pub mod ${snake}_keepables;\`, a \`pub use\`, and registry entry in \`policies/mulligan/mod.rs\`.
  7. Add config-routed score constants to \`PolicyPenalties\` with \`TODO(tune)\` rationale comments.
  8. Add feature calibration, policy verdict, mulligan, and \`cargo ai-gate --quick\` evidence before review.
CHECKLIST
