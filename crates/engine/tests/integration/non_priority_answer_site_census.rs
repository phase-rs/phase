//! THE GATE: every arm of `apply_non_priority_pass_action` that answers a prompt a
//! resolving effect can raise opens the paused-clause attribution bracket, and no other arm
//! does.
//!
//! # The rule this census enforces
//!
//! A `player_scope` clause that pauses on a seat's choice owes ONE result (CR 608.2f +
//! CR 101.4), and its detached tail reads that result (CR 608.2c). Work counts for the clause
//! by frame of origin: an answer to the prompt the clause itself raised is clause work, so
//! the arm that dispatches that answer runs its handler inside
//! `effects::attribute_to_paused_clause` with `ClauseSite::Answer`. The bracket opens only
//! when the standing prompt is the exact prompt the clause recorded, so bracketing an arm
//! whose prompt a resolving effect merely *can* raise is harmless; leaving such an arm
//! unbracketed silently drops that seat's work from the clause.
//!
//! Every arm is classified, in this order:
//!
//! * **B**: the action is a mana ability (CR 605.3b: it resolves immediately, outside the
//!   stack, and is never clause work), or `ActivateAbility` answered only under a mana
//!   payment prompt. Never bracketed, whatever the prompt.
//! * **D**: the head names no `WaitingFor` variant and its guard calls
//!   `engine_resolution_choices::handles`. The bracket lives inside the handler
//!   `handle_resolution_choice`, so the census requires it there, not at the arm.
//! * **E**: the head names no `WaitingFor` variant and is not D (the error fallthrough).
//! * otherwise each named variant is looked up in [`PROMPTS`]: an unlisted variant is a
//!   finding; any [`Raised::DuringResolution`] variant makes the arm **A**; otherwise **C**.
//!
//! # The four checks
//!
//! * **(a)** an arm names a `WaitingFor` variant [`PROMPTS`] does not list, or a row no arm
//!   names (stale).
//! * **(b)** a class-A arm body, or the class-D handler body, does not call the bracket.
//! * **(c)** a class-B, C, D or E arm body calls the bracket.
//! * **(d)** for every [`Raised::Never`] row, the set of `(file, enclosing fn)` production
//!   mentions of `WaitingFor::<Variant>` under `src/game/` must equal the row's allowlist
//!   exactly. A new site fails until it is adjudicated (a resolution-time site means the
//!   row is really [`Raised::DuringResolution`] and its arm must be bracketed); a vanished
//!   site fails too.
//!
//! Arm and handler bodies are read as TOKENS through `syn` (only a parser can say where a
//! match arm ends, and tokenizing drops comments and turns string literals into opaque
//! literals), so a needle in a comment or a string never satisfies (b) and never trips (c).
//! Part (d) reads production text through `source_census` (comment halves removed, inline
//! `#[cfg(test)]` modules cut brace-matched, test-only module files skipped, and the dispatch
//! fn's own span removed so `engine.rs` is not a blind spot).
//!
//! # Limits (labelled residuals)
//!
//! * Part (d) is a PER-SITE census, not a reachability analysis. A new resolution-time
//!   CALLER of an already-allowlisted constructor fn escapes it (for example an effect that
//!   calls a combat or untap-step helper which builds a class-C prompt), and so does a new
//!   constructor added inside an already-allowlisted `(file, fn)`. The allowlisted fns are
//!   turn-structure, combat, setup, shortcut, priority-time activation and projection code,
//!   where a resolution-time constructor would be out of place.
//! * Part (d) walks `src/game/` only. Class-C mentions outside it (`ai_support/`,
//!   `analysis/`, `types/`) are not read; at the time this census was written every such
//!   mention was a read-only projection or inside `#[cfg(test)]`.
//! * The enclosing fn of a mention is the nearest preceding `fn <name>` token in the
//!   production text: a heuristic, not a call-graph fact. A mention that follows a nested fn,
//!   or sits in an item after a fn, is keyed to that fn's name. The key stays stable, so the
//!   allowlist still pins the site; read an allowlist entry as "this text region", not "this
//!   function constructs the prompt".
//!
//! # How this file fails
//!
//! Findings are collected into one `Vec<String>` and asserted once, each naming the arm (its
//! index, variants and actions) or the site, so one run shows every finding.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use syn::ext::IdentExt;
use syn::parse::{ParseStream, Parser};

use crate::source_census;

const ENGINE: &str = include_str!("../../src/game/engine.rs");
const RESOLUTION_CHOICES: &str = include_str!("../../src/game/engine_resolution_choices.rs");
const DISPATCH_FN: &str = "apply_non_priority_pass_action";
const DISPATCH_SCRUTINEE: &str = "&state.waiting_for.clone(), action";
const CLASS_D_HANDLER: &str = "handle_resolution_choice";
const BRACKET: &str = "attribute_to_paused_clause";

/// Every `(file under src/, enclosing fn)` production mention of one variant.
type SiteAllowlist = &'static [(&'static str, &'static str)];

/// When a `WaitingFor` variant can stand.
enum Raised {
    /// A resolving effect can raise it (its arm is an answer site). The reason names how.
    DuringResolution(&'static str),
    /// Only game setup, turn structure, combat, shortcuts or priority-time activation raise
    /// it. `sites` is every `(file under src/, enclosing fn)` production mention.
    Never {
        reason: &'static str,
        sites: SiteAllowlist,
    },
    /// The resting state every resolution returns to; a clause never records it as the
    /// prompt it raised, so the gate could never open on it.
    RestingState(&'static str),
}

impl Raised {
    /// Why the row is classified as it is; every finding about the row cites it.
    fn reason(&self) -> &'static str {
        match self {
            Raised::DuringResolution(reason)
            | Raised::Never { reason, .. }
            | Raised::RestingState(reason) => reason,
        }
    }
}

const DUAL_ORIGIN_CAST: &str =
    "dual-origin: a resolving cast-from-zone effect casts during resolution (CR 608.2g)";

/// Every `WaitingFor` variant the dispatch names, with when it can stand.
const PROMPTS: &[(&str, Raised)] = &[
    (
        "Priority",
        Raised::RestingState("every resolution returns to priority"),
    ),
    (
        "OptionalEffectChoice",
        Raised::DuringResolution("optional instruction (CR 608.2d)"),
    ),
    (
        "OpponentMayChoice",
        Raised::DuringResolution("opponent-may instruction (CR 608.2d)"),
    ),
    (
        "TributeChoice",
        Raised::DuringResolution("tribute resolution"),
    ),
    (
        "UnlessPayment",
        Raised::DuringResolution("unless cost (CR 118.12a)"),
    ),
    (
        "UnlessPaymentChooseCost",
        Raised::DuringResolution("unless cost (CR 118.12a)"),
    ),
    (
        "UnlessBounceChoice",
        Raised::DuringResolution("unless cost (CR 118.12a)"),
    ),
    (
        "ReplacementChoice",
        Raised::DuringResolution("replacement on an effect's event (CR 616.1)"),
    ),
    (
        "EntryControllerChoice",
        Raised::DuringResolution("replacement on an effect's zone move"),
    ),
    (
        "CopyTargetChoice",
        Raised::DuringResolution("copy entry choice on an effect's zone move"),
    ),
    (
        "ReturnAsAuraTarget",
        Raised::DuringResolution("effect zone move"),
    ),
    (
        "ExploreChoice",
        Raised::DuringResolution("explore instruction (CR 701.44)"),
    ),
    (
        "CopyRetarget",
        Raised::DuringResolution("copy-spell instruction (CR 707.10c)"),
    ),
    (
        "MiracleReveal",
        Raised::DuringResolution("dual-origin: a draw during resolution"),
    ),
    (
        "ChooseManaColor",
        Raised::DuringResolution("dual-origin: ManaChoiceContext::ResolvingEffect"),
    ),
    (
        "ProliferateChoice",
        Raised::DuringResolution("proliferate instruction"),
    ),
    (
        "TimeTravelChoice",
        Raised::DuringResolution("time travel instruction"),
    ),
    (
        "ChooseObjectsSelection",
        Raised::DuringResolution("choose-objects instruction"),
    ),
    (
        "DistributeAmong",
        Raised::DuringResolution("dual-origin: casting or resolution-time distribution"),
    ),
    (
        "MoveCountersDistribution",
        Raised::DuringResolution("move-counters instruction"),
    ),
    (
        "RemoveCountersChoice",
        Raised::DuringResolution("remove-counters instruction"),
    ),
    (
        "RetargetChoice",
        Raised::DuringResolution("change-targets instruction"),
    ),
    (
        "MultiTargetSelection",
        Raised::DuringResolution("dual-origin: resolution-time targets"),
    ),
    (
        "AbilityModeChoice",
        Raised::DuringResolution("dual-origin: modal ability chosen during resolution"),
    ),
    ("PairChoice", Raised::DuringResolution("pair instruction")),
    (
        "CollectEvidenceChoice",
        Raised::DuringResolution("dual-origin: CollectEvidenceResume::Effect"),
    ),
    (
        "PayCost",
        Raised::DuringResolution("dual-origin: Effect::PayCost staged payment"),
    ),
    (
        "WardDiscardChoice",
        Raised::DuringResolution("ward trigger resolving (CR 702.21a)"),
    ),
    (
        "WardSacrificeChoice",
        Raised::DuringResolution("ward trigger resolving (CR 702.21a)"),
    ),
    (
        "OrderTriggers",
        Raised::DuringResolution("constructed by a resolving effect"),
    ),
    (
        "TriggerTargetSelection",
        Raised::DuringResolution("constructed under game/effects/"),
    ),
    (
        "ResolveAllReady",
        Raised::DuringResolution("constructed by a resolving cast effect"),
    ),
    (
        "CastOffer",
        Raised::DuringResolution("dual-origin: resolution-time cast offer"),
    ),
    (
        "TargetSelection",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    ("ManaPayment", Raised::DuringResolution(DUAL_ORIGIN_CAST)),
    (
        "ManaSourceSelection",
        Raised::DuringResolution("dual-origin: resolution-time cost"),
    ),
    (
        "PayManaAbilityMana",
        Raised::DuringResolution("dual-origin; its only arm is a mana ability (class B)"),
    ),
    ("ChooseXValue", Raised::DuringResolution(DUAL_ORIGIN_CAST)),
    ("ModeChoice", Raised::DuringResolution(DUAL_ORIGIN_CAST)),
    (
        "ModalFaceChoice",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    (
        "OptionalCostChoice",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    ("CostTypeChoice", Raised::DuringResolution(DUAL_ORIGIN_CAST)),
    (
        "OrderCostReductions",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    (
        "ActivationCostOneOfChoice",
        Raised::DuringResolution("dual-origin: a cost choice a resolving cast or cost reaches"),
    ),
    (
        "AlternativeCastChoice",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    (
        "CastingVariantChoice",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    (
        "ChooseAnnouncingOpponent",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    (
        "ChoosePermanentTypeSlot",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    (
        "ChooseGiftRecipient",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    (
        "PhyrexianPayment",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    ("DefilerPayment", Raised::DuringResolution(DUAL_ORIGIN_CAST)),
    (
        "AssistChoosePlayer",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    ("AssistPayment", Raised::DuringResolution(DUAL_ORIGIN_CAST)),
    ("SpliceOffer", Raised::DuringResolution(DUAL_ORIGIN_CAST)),
    (
        "HarmonizeTapChoice",
        Raised::DuringResolution(DUAL_ORIGIN_CAST),
    ),
    (
        "BlightChoice",
        Raised::DuringResolution("dual-origin: a cost paid during resolution"),
    ),
    (
        "MulliganDecision",
        Raised::Never {
            reason: "game setup (CR 103.5)",
            sites: &[
                ("game/elimination.rs", "prune_mulligan_pending"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/mulligan.rs", "advance_after_decision"),
                ("game/mulligan.rs", "free_reveal_offered_to"),
                ("game/mulligan.rs", "handle_mulligan_bottom"),
                ("game/mulligan.rs", "handle_mulligan_decision"),
                ("game/mulligan.rs", "normal_mulligan_decision"),
                ("game/mulligan.rs", "serum_powders_offered_to"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "OpeningHandBottomCards",
        Raised::Never {
            reason: "game setup (CR 103.5)",
            sites: &[
                ("game/elimination.rs", "prune_mulligan_pending"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/mulligan.rs", "handle_opening_hand_bottom"),
                ("game/mulligan.rs", "start_mulligan"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "BetweenGamesSideboard",
        Raised::Never {
            reason: "between games of a match",
            sites: &[
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/interaction.rs", "sideboard_projection"),
                ("game/match_flow.rs", "between_games_sideboard_prompt"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "filter_state_for_scope"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "BetweenGamesChoosePlayDraw",
        Raised::Never {
            reason: "between games of a match",
            sites: &[
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/match_flow.rs", "handle_submit_sideboard"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "CompanionReveal",
        Raised::Never {
            reason: "pregame companion reveal",
            sites: &[
                ("game/companion.rs", "check_companion_reveal"),
                ("game/companion.rs", "handle_declare_companion"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "filter_state_for_scope"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "DeclareAttackers",
        Raised::Never {
            reason: "combat declaration (CR 508.1)",
            sites: &[
                ("game/combat.rs", "build_declare_attackers_waiting_for"),
                ("game/combat.rs", "refresh_combat_declaration_waiting_for"),
                ("game/engine.rs", "run_auto_pass_loop"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "combat_relation_projection"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/public_state.rs", "step_bound_phase"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "DeclareBlockers",
        Raised::Never {
            reason: "combat declaration (CR 509.1)",
            sites: &[
                ("game/combat.rs", "build_declare_blockers_waiting_for"),
                ("game/combat.rs", "refresh_combat_declaration_waiting_for"),
                ("game/engine.rs", "run_auto_pass_loop"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "combat_relation_projection"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/public_state.rs", "step_bound_phase"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "ExertChoice",
        Raised::Never {
            reason: "attack declaration (CR 508.1)",
            sites: &[
                (
                    "game/engine_combat.rs",
                    "continue_declare_attackers_after_commit",
                ),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "EnlistChoice",
        Raised::Never {
            reason: "attack declaration (CR 508.1)",
            sites: &[
                ("game/engine_combat.rs", "next_enlist_choice"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "CombatTaxPayment",
        Raised::Never {
            reason: "attack or block declaration cost (CR 508.1h)",
            sites: &[
                ("game/combat.rs", "pending_combat_tax_is_affordable"),
                ("game/engine_combat.rs", "handle_declare_attackers"),
                ("game/engine_combat.rs", "handle_declare_blockers"),
                ("game/engine_combat.rs", "handle_pay_combat_tax"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "AssignCombatDamage",
        Raised::Never {
            reason: "combat damage step (CR 510.1)",
            sites: &[
                ("game/combat_damage.rs", "assess_combat_impact"),
                ("game/combat_damage.rs", "collect_damage_assignments"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "damage_assignment_projection"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "opportunity_for_slot"),
                ("game/interaction.rs", "selection_projection"),
                ("game/public_state.rs", "step_bound_phase"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "AssignBlockerDamage",
        Raised::Never {
            reason: "combat damage step (CR 510.1)",
            sites: &[
                ("game/combat_damage.rs", "assess_combat_impact"),
                ("game/combat_damage.rs", "collect_damage_assignments"),
                ("game/interaction.rs", "amount_assignment_projection"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "opportunity_for_slot"),
                ("game/interaction.rs", "selection_projection"),
                ("game/public_state.rs", "step_bound_phase"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "UntapChoice",
        Raised::Never {
            reason: "untap step (CR 502.3)",
            sites: &[
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "direct_choice_projection"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/turns.rs", "auto_advance_once"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "ChooseUntapSubset",
        Raised::Never {
            reason: "untap step (CR 502.3)",
            sites: &[
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/turns.rs", "begin_untap_or_subset_prompt"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "LoopShortcut",
        Raised::Never {
            reason: "priority-time shortcut (CR 732.1)",
            sites: &[
                ("game/derived_views.rs", "derive_views"),
                ("game/engine.rs", "certified_bounded_cycle_offer"),
                ("game/engine.rs", "interactive_loop_bridge"),
                ("game/engine.rs", "reconcile_terminal_result"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "loop_shortcut_projection"),
                ("game/interaction.rs", "materialize_response"),
                ("game/interaction.rs", "selection_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "filter_state_for_scope"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "RespondToShortcut",
        Raised::Never {
            reason: "priority-time shortcut (CR 732.1)",
            sites: &[
                ("game/engine.rs", "handle_declare_shortcut"),
                ("game/engine.rs", "handle_respond_to_shortcut"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "declared_shortcut_projection"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/interaction.rs", "shortcut_reply_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "filter_state_for_scope"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "PrecastCopyShortcutOffer",
        Raised::Never {
            reason: "priority-time shortcut (CR 732.1)",
            sites: &[
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "direct_choice_projection"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/precast_copy_shortcut.rs", "handle"),
                (
                    "game/precast_copy_shortcut.rs",
                    "maybe_offer_after_cast_triggers",
                ),
                (
                    "game/precast_copy_shortcut.rs",
                    "normalize_untrusted_restore",
                ),
                (
                    "game/precast_copy_shortcut.rs",
                    "rekey_after_trusted_restore",
                ),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "RespondToPrecastCopyShortcut",
        Raised::Never {
            reason: "priority-time shortcut (CR 732.1)",
            sites: &[
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "direct_choice_projection"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/precast_copy_shortcut.rs", "handle"),
                (
                    "game/precast_copy_shortcut.rs",
                    "normalize_untrusted_restore",
                ),
                (
                    "game/precast_copy_shortcut.rs",
                    "rekey_after_trusted_restore",
                ),
                ("game/precast_copy_shortcut.rs", "responder_wait"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "ResolveAllConsent",
        Raised::Never {
            reason: "priority-time resolve-all consent",
            sites: &[
                ("game/engine.rs", "materialize_live_resolve_all_session"),
                ("game/engine.rs", "resolve_all_consent_waiting_for"),
                (
                    "game/engine_resolve_batch.rs",
                    "classify_restored_stack_automation",
                ),
                (
                    "game/engine_resolve_batch.rs",
                    "repair_restored_stack_automation",
                ),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "interaction_submitter_for_owner"),
                ("game/interaction.rs", "selection_projection"),
                ("game/interaction.rs", "semantic_slots"),
                (
                    "game/public_state.rs",
                    "sync_priority_player_from_waiting_for",
                ),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/turn_control.rs", "authorized_submitter_for_player"),
                (
                    "game/turn_control.rs",
                    "invalidate_resolve_all_consent_inner",
                ),
                ("game/turn_control.rs", "resolve_all_granted_submitter"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "CrewVehicle",
        Raised::Never {
            reason: "activation announced at priority (CR 602.2)",
            sites: &[
                ("game/engine.rs", "activation_cost_still_open"),
                ("game/engine.rs", "handle_crew_activation"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_power"),
                ("game/interaction.rs", "selection_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "SaddleMount",
        Raised::Never {
            reason: "activation announced at priority (CR 602.2)",
            sites: &[
                ("game/engine.rs", "activation_cost_still_open"),
                ("game/engine.rs", "handle_saddle_activation"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_power"),
                ("game/interaction.rs", "selection_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "StationTarget",
        Raised::Never {
            reason: "activation announced at priority (CR 602.2)",
            sites: &[
                ("game/engine.rs", "activation_cost_still_open"),
                ("game/engine.rs", "handle_station_activation"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
    (
        "EquipTarget",
        Raised::Never {
            reason: "activation announced at priority (CR 602.2)",
            sites: &[
                ("game/engine.rs", "activation_cost_still_open"),
                ("game/engine.rs", "handle_equip_activation"),
                ("game/interaction.rs", "classify_waiting_for"),
                ("game/interaction.rs", "human_response_model"),
                ("game/interaction.rs", "selection_projection"),
                ("game/scenario.rs", "waiting_for_kind"),
                ("game/visibility.rs", "redact_paid_cast_cleanup_authority"),
            ],
        },
    ),
];

/// The actions of a mana ability (CR 605.3b).
const MANA_ACTIONS: &[&str] = &[
    "TapLandForMana",
    "UntapLandForMana",
    "ActivateManaSource",
    "SpendPoolMana",
    "UnspendPoolMana",
    "TapForConvoke",
    "PayManaAbilityMana",
];
/// The prompts under which `ActivateAbility` is a mana ability activated mid-payment
/// (CR 605.3a).
const MANA_ACTIVATE_UNDER: &[&str] = &["UnlessPayment", "ManaPayment", "ManaSourceSelection"];

// ── Token reader ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum Delimiter {
    Parenthesis,
    Brace,
    Bracket,
}

/// A source token with comments dropped and every literal made opaque.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Punct(char),
    Literal,
    Group(Delimiter, Vec<Token>),
}

fn read_one(input: ParseStream) -> syn::Result<Token> {
    if input.peek(syn::token::Paren) {
        let inner;
        syn::parenthesized!(inner in input);
        return Ok(Token::Group(Delimiter::Parenthesis, read_tokens(&inner)?));
    }
    if input.peek(syn::token::Brace) {
        let inner;
        syn::braced!(inner in input);
        return Ok(Token::Group(Delimiter::Brace, read_tokens(&inner)?));
    }
    if input.peek(syn::token::Bracket) {
        let inner;
        syn::bracketed!(inner in input);
        return Ok(Token::Group(Delimiter::Bracket, read_tokens(&inner)?));
    }
    if input.peek(syn::Lifetime) {
        input.parse::<syn::Lifetime>()?;
        return Ok(Token::Literal);
    }
    if input.peek(syn::Lit) {
        input.parse::<syn::Lit>()?;
        return Ok(Token::Literal);
    }
    if input.peek(syn::Ident::peek_any) {
        return Ok(Token::Ident(syn::Ident::parse_any(input)?.to_string()));
    }
    let punct = input.step(|cursor| match cursor.punct() {
        Some((punct, rest)) => Ok((punct.as_char(), rest)),
        None => Err(cursor.error("unexpected token")),
    })?;
    Ok(Token::Punct(punct))
}

fn read_tokens(input: ParseStream) -> syn::Result<Vec<Token>> {
    let mut out = Vec::new();
    while !input.is_empty() {
        out.push(read_one(input)?);
    }
    Ok(out)
}

fn tokenize(source: &str) -> Vec<Token> {
    read_tokens.parse_str(source).expect("the source tokenizes")
}

/// Every `<enum_name>::<Variant>` path segment, at any depth.
fn variants_after(tokens: &[Token], enum_name: &str, out: &mut Vec<String>) {
    for (index, token) in tokens.iter().enumerate() {
        match token {
            Token::Group(_, inner) => variants_after(inner, enum_name, out),
            Token::Ident(name) if name == enum_name => {
                if let [Token::Punct(':'), Token::Punct(':'), Token::Ident(variant), ..] =
                    &tokens[index + 1..]
                {
                    out.push(variant.clone());
                }
            }
            _ => {}
        }
    }
}

fn mentions_ident(tokens: &[Token], ident: &str) -> bool {
    tokens.iter().any(|token| match token {
        Token::Ident(name) => name == ident,
        Token::Group(_, inner) => mentions_ident(inner, ident),
        _ => false,
    })
}

/// Reads top-level tokens up to `fn <name>`, then returns that fn's body group. Groups are
/// consumed whole, so a same-named fn inside a module (a test module) is never matched.
fn read_to_fn_body(input: ParseStream, name: &str) -> syn::Result<Vec<Token>> {
    loop {
        if input.is_empty() {
            return Err(input.error(format!("fn {name} not found at top level")));
        }
        if read_one(input)? == Token::Ident("fn".into())
            && input
                .fork()
                .parse::<syn::Ident>()
                .is_ok_and(|ident| ident == name)
        {
            break;
        }
    }
    while !input.peek(syn::token::Brace) {
        read_one(input)?;
    }
    let body;
    syn::braced!(body in input);
    let tokens = read_tokens(&body)?;
    while !input.is_empty() {
        read_one(input)?;
    }
    Ok(tokens)
}

// ── Arms ──────────────────────────────────────────────────────────────────────

struct Arm {
    head: Vec<Token>,
    body: Vec<Token>,
}

/// `syn` decides where each arm ends; the arm's tokens are re-read from a fork.
fn parse_arms(input: ParseStream) -> syn::Result<Vec<Arm>> {
    let mut arms = Vec::new();
    while !input.is_empty() {
        let reader = input.fork();
        input.parse::<syn::Arm>()?;
        let mut tokens = Vec::new();
        while reader.cursor() != input.cursor() {
            tokens.push(read_one(&reader)?);
        }
        let arrow = tokens
            .windows(2)
            .position(|pair| pair == [Token::Punct('='), Token::Punct('>')])
            .expect("an arm has `=>`");
        let body = tokens.split_off(arrow + 2);
        tokens.truncate(arrow);
        arms.push(Arm { head: tokens, body });
    }
    Ok(arms)
}

fn find_dispatch(input: ParseStream) -> syn::Result<Vec<Vec<Arm>>> {
    let scrutinee = tokenize(DISPATCH_SCRUTINEE);
    loop {
        if input.is_empty() {
            return Err(input.error("dispatch fn not found"));
        }
        if read_one(input)? == Token::Ident("fn".into())
            && input
                .fork()
                .parse::<syn::Ident>()
                .is_ok_and(|name| name == DISPATCH_FN)
        {
            break;
        }
    }
    while !input.peek(syn::token::Brace) {
        read_one(input)?;
    }
    let body;
    syn::braced!(body in input);
    let mut found = Vec::new();
    while !body.is_empty() {
        if read_one(&body)? != Token::Ident("match".into()) {
            continue;
        }
        let ahead = body.fork();
        let is_dispatch = matches!(
            read_one(&ahead),
            Ok(Token::Group(Delimiter::Parenthesis, ref head)) if *head == scrutinee
        ) && ahead.peek(syn::token::Brace);
        if is_dispatch {
            read_one(&body)?;
            let arms;
            syn::braced!(arms in body);
            found.push(parse_arms(&arms)?);
        }
    }
    while !input.is_empty() {
        read_one(input)?;
    }
    Ok(found)
}

fn dispatch_arms(source: &str) -> Vec<Arm> {
    let mut found = find_dispatch
        .parse_str(source)
        .expect("the dispatch source parses");
    assert_eq!(
        found.len(),
        1,
        "exactly one dispatch match in {DISPATCH_FN}"
    );
    found.pop().expect("one dispatch match")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    A,
    B,
    C,
    D,
    E,
}

struct ClassifiedArm {
    index: usize,
    class: Option<Class>,
    prompts: Vec<String>,
    actions: Vec<String>,
    unknown: Vec<String>,
    calls_bracket: bool,
}

impl ClassifiedArm {
    fn label(&self) -> String {
        format!("arm {} {:?}/{:?}", self.index, self.prompts, self.actions)
    }
}

fn row(variant: &str) -> Option<&'static Raised> {
    PROMPTS
        .iter()
        .find(|(name, _)| *name == variant)
        .map(|(_, raised)| raised)
}

fn classify(index: usize, arm: &Arm) -> ClassifiedArm {
    let mut prompts = Vec::new();
    variants_after(&arm.head, "WaitingFor", &mut prompts);
    let mut actions = Vec::new();
    variants_after(&arm.head, "GameAction", &mut actions);
    let is_mana_action = actions
        .iter()
        .any(|action| MANA_ACTIONS.contains(&action.as_str()))
        || (actions == ["ActivateAbility"]
            && !prompts.is_empty()
            && prompts
                .iter()
                .all(|prompt| MANA_ACTIVATE_UNDER.contains(&prompt.as_str())));
    let unknown: Vec<String> = prompts
        .iter()
        .filter(|prompt| row(prompt).is_none())
        .cloned()
        .collect();
    let class = if is_mana_action {
        Some(Class::B)
    } else if prompts.is_empty() {
        if mentions_ident(&arm.head, "handles") {
            Some(Class::D)
        } else {
            Some(Class::E)
        }
    } else if !unknown.is_empty() {
        None
    } else if prompts
        .iter()
        .any(|prompt| matches!(row(prompt), Some(Raised::DuringResolution(_))))
    {
        Some(Class::A)
    } else {
        Some(Class::C)
    };
    ClassifiedArm {
        index,
        class,
        prompts,
        actions,
        unknown,
        calls_bracket: mentions_ident(&arm.body, BRACKET),
    }
}

/// Parts (a)-unknown, (b) and (c) over one dispatch source, plus (b) on the class-D handler.
fn arm_findings(dispatch_source: &str, handler_source: &str) -> (Vec<ClassifiedArm>, Vec<String>) {
    let arms: Vec<ClassifiedArm> = dispatch_arms(dispatch_source)
        .iter()
        .enumerate()
        .map(|(index, arm)| classify(index, arm))
        .collect();
    let mut findings = Vec::new();
    for arm in &arms {
        match (arm.class, arm.calls_bracket) {
            (None, _) => findings.push(format!(
                "(a) {} names WaitingFor variants with no PROMPTS row: {:?}",
                arm.label(),
                arm.unknown
            )),
            (Some(Class::A), false) => {
                let reasons: Vec<&str> = arm
                    .prompts
                    .iter()
                    .filter_map(|prompt| row(prompt))
                    .filter(|raised| matches!(raised, Raised::DuringResolution(_)))
                    .map(Raised::reason)
                    .collect();
                findings.push(format!(
                    "(b) class-A {} does not call {BRACKET} (raised during resolution: {})",
                    arm.label(),
                    reasons.join("; ")
                ));
            }
            (Some(Class::A), true) => {}
            (Some(class), true) => findings.push(format!(
                "(c) class-{class:?} {} calls {BRACKET}",
                arm.label()
            )),
            (Some(_), false) => {}
        }
    }
    let handler = (|input: ParseStream| read_to_fn_body(input, CLASS_D_HANDLER))
        .parse_str(handler_source)
        .map(|tokens| mentions_ident(&tokens, BRACKET));
    match handler {
        Ok(true) => {}
        Ok(false) => findings.push(format!(
            "(b) class-D handler {CLASS_D_HANDLER} does not call {BRACKET}"
        )),
        Err(error) => findings.push(format!("(b) class-D handler unreadable: {error}")),
    }
    (arms, findings)
}

// ── Part (d): mention sites ───────────────────────────────────────────────────

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {dir:?}: {error}")) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// The index one past the `}` matching the `{` at `open`, skipping string and char
/// literals.
fn matching_brace(code: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut index = open;
    while index < code.len() {
        match code[index] {
            b'"' => {
                index += 1;
                while index < code.len() && code[index] != b'"' {
                    if code[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
            }
            b'\'' if index + 2 < code.len() && code[index + 2] == b'\'' => index += 2,
            b'\''
                if index + 3 < code.len()
                    && code[index + 1] == b'\\'
                    && code[index + 3] == b'\'' =>
            {
                index += 3
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return index + 1;
                }
            }
            _ => {}
        }
        index += 1;
    }
    code.len()
}

/// One file's production text: comment halves removed (`source_census::code_lines`) and
/// every inline `#[cfg(test)]` module cut out brace-matched.
fn production_code(src: &str) -> String {
    let code = source_census::code_lines(src);
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len());
    let mut index = 0;
    while let Some(at) = code[index..].find("#[cfg(test)]") {
        let attr = index + at;
        out.push_str(&code[index..attr]);
        let rest = &code[attr..];
        let item = rest
            .lines()
            .skip(1)
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("#["))
            .unwrap_or("");
        let is_inline_mod = ["mod ", "pub mod ", "pub(crate) mod ", "pub(super) mod "]
            .iter()
            .any(|prefix| item.starts_with(prefix))
            && item.ends_with('{');
        if is_inline_mod {
            let open = attr + rest.find('{').expect("an inline module opens a brace");
            index = matching_brace(bytes, open);
        } else {
            out.push_str("#[cfg(test)]");
            index = attr + "#[cfg(test)]".len();
        }
    }
    out.push_str(&code[index..]);
    out
}

/// Files declared `#[cfg(test)] mod m;` (optionally with `#[path = "…"]`): test-only.
fn test_only_files(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(file).expect("source file reads");
        let lines: Vec<&str> = text.lines().map(source_census::code).collect();
        for (index, line) in lines.iter().enumerate() {
            if line.trim() != "#[cfg(test)]" {
                continue;
            }
            let mut path_attr = None;
            for next in lines.iter().skip(index + 1).map(|line| line.trim()) {
                if let Some(rest) = next.strip_prefix("#[path = \"") {
                    path_attr = rest.strip_suffix("\"]").map(str::to_string);
                    continue;
                }
                if next.starts_with("#[") || next.is_empty() {
                    continue;
                }
                let module = next
                    .strip_prefix("pub(crate) mod ")
                    .or_else(|| next.strip_prefix("mod "))
                    .and_then(|module| module.strip_suffix(';'));
                if let Some(module) = module {
                    let dir = file.parent().expect("a source file has a parent");
                    let is_dir_owner = file
                        .file_name()
                        .is_some_and(|name| name == "mod.rs" || name == "lib.rs");
                    let stem_dir = if is_dir_owner {
                        dir.to_path_buf()
                    } else {
                        dir.join(file.file_stem().expect("a source file has a stem"))
                    };
                    match &path_attr {
                        Some(path) => out.push(dir.join(path)),
                        None => {
                            out.push(stem_dir.join(format!("{module}.rs")));
                            out.push(dir.join(format!("{module}.rs")));
                        }
                    }
                }
                break;
            }
        }
    }
    out
}

/// The nearest preceding `fn <name>` in `before` (a heuristic; see the module doc).
fn enclosing_fn(before: &str) -> String {
    before
        .rmatch_indices("fn ")
        .find_map(|(index, _)| {
            let name: String = before[index + 3..]
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            let starts_a_word = before[..index]
                .chars()
                .next_back()
                .is_none_or(|character| !(character.is_alphanumeric() || character == '_'));
            (!name.is_empty() && starts_a_word).then_some(name)
        })
        .unwrap_or_default()
}

struct SiteWalk {
    /// `variant -> {(file under src/, enclosing fn)}`.
    sites: BTreeMap<String, BTreeSet<(String, String)>>,
    /// Every production file read, relative to `src/`.
    visited: BTreeSet<String>,
}

fn mention_sites(variants: &[&str]) -> SiteWalk {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src_root.join("game"), &mut files);
    files.sort();
    let mut all = Vec::new();
    rs_files(&src_root, &mut all);
    let test_only = test_only_files(&all);
    let mut walk = SiteWalk {
        sites: BTreeMap::new(),
        visited: BTreeSet::new(),
    };
    for file in &files {
        if test_only.iter().any(|test_file| test_file == file) {
            continue;
        }
        let relative = file
            .strip_prefix(&src_root)
            .expect("a walked file is under src/")
            .to_string_lossy()
            .replace('\\', "/");
        let mut code = production_code(&std::fs::read_to_string(file).expect("source reads"));
        if relative == "game/engine.rs" {
            let start = code
                .find(&format!("fn {DISPATCH_FN}("))
                .expect("the dispatch fn is production code");
            let open = start + code[start..].find('{').expect("the dispatch fn has a body");
            let end = matching_brace(code.as_bytes(), open);
            code.replace_range(start..end, "");
        }
        if !code.trim().is_empty() {
            walk.visited.insert(relative.clone());
        }
        for variant in variants {
            let needle = format!("WaitingFor::{variant}");
            let mut from = 0;
            while let Some(at) = code[from..].find(&needle) {
                let position = from + at;
                from = position + needle.len();
                let ends_the_name = code[from..]
                    .chars()
                    .next()
                    .is_none_or(|character| !(character.is_alphanumeric() || character == '_'));
                if ends_the_name {
                    walk.sites
                        .entry(variant.to_string())
                        .or_default()
                        .insert((relative.clone(), enclosing_fn(&code[..position])));
                }
            }
        }
    }
    walk
}

// ── The census ────────────────────────────────────────────────────────────────

#[test]
fn every_resolution_time_answer_arm_opens_the_clause_bracket_and_no_other_does() {
    let (arms, mut findings) = arm_findings(ENGINE, RESOLUTION_CHOICES);

    // Reach guard: the dispatch was found and every class is populated, so a parse that
    // silently returned a handful of arms cannot pass.
    let mut counts: BTreeMap<Class, usize> = BTreeMap::new();
    for arm in &arms {
        if let Some(class) = arm.class {
            *counts.entry(class).or_default() += 1;
        }
    }
    for class in [Class::A, Class::B, Class::C, Class::D, Class::E] {
        assert!(
            counts.get(&class).is_some_and(|count| *count > 0),
            "reach guard: no class-{class:?} arm was classified ({counts:?})"
        );
    }

    // (a) stale rows.
    let named: BTreeSet<&str> = arms
        .iter()
        .flat_map(|arm| arm.prompts.iter().map(String::as_str))
        .collect();
    for (variant, raised) in PROMPTS {
        if !named.contains(variant) {
            findings.push(format!(
                "(a) stale PROMPTS row {variant} ({}): no arm names it",
                raised.reason()
            ));
        }
    }

    // (d) class-C mention sites.
    let never: Vec<(&str, &str, SiteAllowlist)> = PROMPTS
        .iter()
        .filter_map(|(variant, raised)| match raised {
            Raised::Never { reason, sites } => Some((*variant, *reason, *sites)),
            Raised::DuringResolution(_) | Raised::RestingState(_) => None,
        })
        .collect();
    let variants: Vec<&str> = never.iter().map(|(variant, _, _)| *variant).collect();
    let walk = mention_sites(&variants);
    // Reach guard: the recursive walk reads production code below `game/`'s top level, so
    // "no site under game/effects/" is a measured zero, not an unvisited directory.
    assert!(
        walk.visited.contains("game/effects/mod.rs"),
        "reach guard: the site walk never read game/effects/mod.rs"
    );
    for (variant, reason, allowed) in &never {
        let found = walk.sites.get(*variant).cloned().unwrap_or_default();
        let allowed: BTreeSet<(String, String)> = allowed
            .iter()
            .map(|(file, function)| (file.to_string(), function.to_string()))
            .collect();
        for (file, function) in found.difference(&allowed) {
            findings.push(format!(
                "(d) new mention of WaitingFor::{variant} ({reason}) at {file}::{function}: \
                 adjudicate it (a resolution-time site makes the row DuringResolution and its arm \
                 an answer site)"
            ));
        }
        for (file, function) in allowed.difference(&found) {
            findings.push(format!(
                "(d) allowlisted mention of WaitingFor::{variant} ({reason}) at {file}::{function} \
                 is gone"
            ));
        }
    }

    assert!(
        findings.is_empty(),
        "answer-site census findings:\n{}",
        findings.join("\n")
    );
}

/// Discrimination: planted dispatch and handler sources, each arm reading as stated.
#[test]
fn planted_arms_and_handlers_are_classified_by_tokens_not_text() {
    let planted = r#"
        fn apply_non_priority_pass_action(state: &mut GameState, action: GameAction) {
            let waiting_for = match (&state.waiting_for.clone(), action) {
                (WaitingFor::OptionalEffectChoice { .. }, GameAction::DecideOptionalEffect { accept }) => {
                    handle(state) // attribute_to_paused_clause(
                }
                (WaitingFor::DeclareAttackers { .. }, GameAction::DeclareAttackers { attacks }) => {
                    attribute_to_paused_clause(state, |s| declare(s))
                }
                (WaitingFor::Priority { .. }, GameAction::TapLandForMana { object_id }) => {
                    tap(state, "attribute_to_paused_clause(")
                }
                (WaitingFor::BrandNewPrompt { .. }, GameAction::Foo) => foo::<A, B>(state, |a, b| a),
                (WaitingFor::ReplacementChoice { .. }, GameAction::ChooseReplacement { index }) => {
                    crate::game::effects::attribute_to_paused_clause(state, |s| bar::<X, Y>(s))
                }
                (waiting_for, action) if engine_resolution_choices::handles(waiting_for) => { x }
                (waiting, action) => { return Err(e) }
            };
        }
    "#;
    let quoted_handler = r#"
        fn handle_resolution_choice(state: &mut GameState) -> Outcome {
            let note = "attribute_to_paused_clause(";
            handle(state, note)
        }
    "#;
    let (arms, findings) = arm_findings(planted, quoted_handler);
    let classes: Vec<Option<Class>> = arms.iter().map(|arm| arm.class).collect();
    assert_eq!(
        classes,
        vec![
            Some(Class::A),
            Some(Class::C),
            Some(Class::B),
            None,
            Some(Class::A),
            Some(Class::D),
            Some(Class::E),
        ],
        "every planted arm classified as planted (a two-parameter turbofish and closure \
         must not split an arm)"
    );
    assert_eq!(
        findings,
        vec![
            "(b) class-A arm 0 [\"OptionalEffectChoice\"]/[\"DecideOptionalEffect\"] does not \
             call attribute_to_paused_clause (raised during resolution: optional instruction (CR \
             608.2d))"
                .to_string(),
            "(c) class-C arm 1 [\"DeclareAttackers\"]/[\"DeclareAttackers\"] calls \
             attribute_to_paused_clause"
                .to_string(),
            "(a) arm 3 [\"BrandNewPrompt\"]/[\"Foo\"] names WaitingFor variants with no PROMPTS \
             row: [\"BrandNewPrompt\"]"
                .to_string(),
            "(b) class-D handler handle_resolution_choice does not call \
             attribute_to_paused_clause"
                .to_string(),
        ],
        "a trailing-comment mention and a string-literal mention never count; a class-C \
         bracket and an unknown variant are caught"
    );

    let calling_handler = r#"
        fn handle_resolution_choice(state: &mut GameState) -> Outcome {
            crate::game::effects::attribute_to_paused_clause(state, |state| answer(state))
        }
    "#;
    let (_, findings) = arm_findings(planted, calling_handler);
    assert!(
        !findings
            .iter()
            .any(|finding| finding.contains("class-D handler")),
        "a handler that calls the bracket is not a finding: {findings:?}"
    );
}
