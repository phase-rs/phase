---
gsd_state_version: 1.0
milestone: v1.2
milestone_name: Migrate Data Source & Add Tests
status: executing
stopped_at: Phase 31 context gathered
last_updated: "2026-03-17T00:26:55.499Z"
last_activity: 2026-03-16 — Phase 30 Plan 01 complete (Building Block Type Definitions)
progress:
  total_phases: 11
  completed_phases: 7
  total_plans: 51
  completed_plans: 47
  percent: 92
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-03-11)

**Core value:** A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent -- with all cards behaving correctly according to MTG comprehensive rules.
**Current focus:** Phase 29 Support N Players — Plan 16 complete (Effects Modules N-Z PlayerId(1-x) Migration)

## Current Position

Phase: 30 — Plan 2 of 4 complete
Status: In Progress
Last activity: 2026-03-17 — Phase 30 Plan 02 complete (Pipeline Plumbing)

Progress: [████████████████░░░░] 47/51 plans (92%)

## Performance Metrics

**Velocity:**
- Total plans completed: 18 (v1.2)
- Average duration: 12min
- Total execution time: 3.5 hours

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 21 | 5/5 | 68min | 14min |
| 22 | 3/3 | 35min | 12min |
| 23 | 2/2 | 18min | 9min |
| 24 | 3/3 | 43min | 14min |
| 25 | 3/3 | 31min | 10min |
| 26 | 6/6 | 36min | 6min |
| 28 | 5/6 | 146min | 29min |
| Phase 26 P01 | 8min | 3 tasks | 16 files |
| Phase 26 P03 | 5min | 3 tasks | 6 files |
| Phase 26 P04 | 7min | 3 tasks | 12 files |
| Phase 26 P05 | 8min | 2 tasks | 15 files |
| Phase 26 P06 | 8min | 3 tasks | 6 files |
| Phase 28 P01 | 10min | 2 tasks | 10 files |
| Phase 28 P02 | 13min | 3 tasks | 5 files |
| Phase 28 P03 | 25min | 3 tasks | 26 files |
| Phase 28 P06 | 90min | 2 tasks | 35 files |
| Phase 28 P04 | 8min | 2 tasks | 30127 files |
| Phase 27 P01 | 11min | 2 tasks | 11 files |
| Phase 27 P02 | 6min | 2 tasks | 2 files |
| Phase 27 P03 | 13min | 2 tasks | 3 files |
| Phase 27 P04 | 2min | 2 tasks | 3 files |
| Phase 29 P01 | 7min | 2 tasks | 6 files |
| Phase 29 P02 | 11min | 2 tasks | 11 files |
| Phase 29 P03 | 25min | 2 tasks | 11 files |
| Phase 29 P05 | 3min | 1 tasks | 3 files |
| Phase 29 P16 | 2min | 1 tasks | 0 files |
| Phase 29 P15 | 4min | 2 tasks | 1 files |
| Phase 29 P14 | 4min | 1 tasks | 1 files |
| Phase 29 P07 | 9min | 3 tasks | 10 files |
| Phase 29 P08 | 6min | 2 tasks | 6 files |
| Phase 29 P10 | 5min | 2 tasks | 11 files |
| Phase 29 P11 | 7min | 2 tasks | 12 files |
| Phase 29 P12 | 15min | 3 tasks | 6 files |
| Phase 30 P01 | 21min | 2 tasks | 13 files |
| Phase 30 P02 | 32min | 2 tasks | 11 files |

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table.
Recent decisions affecting current work:

- [v1.2 init]: Custom MTGJSON types (~50 lines) instead of mtgjson crate (missing defense field, unnecessary transitive deps)
- [v1.2 init]: Author ability definitions from oracle text/rules, use Forge output only for validation (GPL contamination avoidance)
- [Phase 21 context]: REVERSED prior decision — Vec<AbilityDefinition> refactor included in Phase 21 (not deferred). Emitting Forge-compatible strings perpetuates licensing risk. Typed enums for all four definition types (Effect, TriggerMode, StaticMode, ReplacementEvent)
- [Phase 21 context]: AtomicCards.json committed to repo, one ability JSON per card in data/abilities/, schemars for schema generation
- [21-01]: remaining_params field on AbilityDefinition preserves unconsumed parser params for compat
- [21-01]: Display impl on TriggerMode uses Debug formatting for known variants (simple, correct)
- [21-01]: ResolvedAbility left unchanged per plan — transitional approach for Plan 02
- [21-01]: Compat methods (api_type(), params(), mode_str(), event_str()) bridge typed enums to string consumers
- [21-03]: 7-card test fixture instead of full 50MB AtomicCards.json for fast CI
- [21-03]: Schema generation via test keeps schema.json in sync with Rust types
- [21-03]: Relative $schema path in ability JSON files for co-located schema
- [Phase 21-02]: Kept compat bridge methods (api_type, params) for transitional dispatch rather than full pattern-matching migration
- [Phase 21-02]: parse_test_ability() helpers in test modules for readable typed test data construction
- [21-04]: Kept api_type/params on ResolvedAbility for backward compat; typed dispatch via match on effect field
- [21-04]: from_raw() wraps in Effect::Other for test compat; new() builds from typed Effect for production code
- [22-01]: CardBuilder borrows &mut GameState (not &mut GameScenario) to avoid borrow checker conflicts
- [22-01]: scenario.rs not #[cfg(test)] gated -- integration tests compile crate as dependency, can't access cfg(test) modules
- [22-01]: #[path] attributes for integration test module resolution in Cargo test binaries
- [22-02]: ChangesZone triggers test Hand->Stack zone transitions (not just ETB) -- engine fires on all zone changes
- [22-02]: Deathtouch SBA test uses direct GameState construction since GameRunner doesn't expose &mut state for dealt_deathtouch_damage
- [22-02]: Explicit act(PassPriority) loop for stack drain when triggers add entries during resolution
- [22-03]: CardBuilder must push keywords to both keywords and base_keywords to survive layer evaluation (bug fix)
- [22-03]: Combat integration tests use run_combat() helper driving full engine pipeline (PassPriority -> DeclareAttackers -> DeclareBlockers)
- [22-03]: Layer tests trigger evaluation via PassPriority (SBAs run, which evaluate layers when layers_dirty=true)
- [22-03]: GameRunner::snapshot() for step-by-step snapshot tests
- [23-01]: Case-insensitive MTGJSON name matching for filename-to-card lookup (handles title-cased prepositions)
- [23-01]: FaceAbilities struct with flat #[serde(default)] fields mirroring AbilityFile for multi-face cards
- [23-01]: Equip synthesis uses Effect::Attach with TargetSpec::Filtered for Creature.YouCtrl
- [23-01]: CardDatabase fields made pub(crate) for cross-module construction by json_loader
- [23-02]: normalize_for_match() strips punctuation for card name matching (handles comma in "Jace, the Mind Sculptor")
- [23-02]: Smoke game tests use direct mana pool injection for spell casting, PassPriority loop for combat phases
- [23-02]: Ability JSON files use Effect::Other for complex card-specific effects not yet covered by typed variants
- [24-01]: Unknown cost components (PayLife, Discard, tapXType, exert) preserved as AbilityCost::Mana fallback (matches Effect::Other pattern)
- [24-01]: Migration overwrites 8 hand-authored JSON files from Phase 23 for consistency across all 32,274 cards
- [24-01]: json_smoke_test adapted: error-type checking (not zero errors) and fixture-centric cross-validation
- [24-01]: 26 Forge files with missing Name field skipped as errors (Specialize variants with alternate format)
- [24-02]: Keyword parity compares base names (before colon) -- Forge "Ward:1" matches MTGJSON "Ward"
- [24-02]: NoCost and Cost{0,[]} treated as equivalent for basic land mana cost comparison
- [24-02]: Extra MTGJSON-only keywords (Scry, Mill) allowed -- action keywords Forge doesn't track
- [24-02]: normalize_for_match strips all non-alphanumeric chars (fixes apostrophe card matching)
- [24-03]: Benign MTGJSON keyword mismatches stripped in coverage binary (bare parameterized keywords, action keywords like Scry/Mill)
- [24-03]: Coverage manifest filtering applied after analyze_standard_coverage() -- binary-local logic, coverage.rs unchanged
- [24-03]: MTGJSON action keywords (Scry, Mill, Surveil, Fateseal) excluded from coverage since handled as effects, not keywords
- [25-01]: effect_variant_name() as standalone function for production variant-to-string mapping (not a method on Effect)
- [25-01]: Effect::to_params() stays ungated -- legitimate typed-to-HashMap serialization for SubAbility chains
- [25-01]: Test code retains compat bridge methods (api_type, params, from_raw) -- gated in Plan 02
- [25-02]: ResolvedAbility::from_raw() kept ungated -- used in production (triggers.rs empty-effect fallback)
- [25-02]: card-data-export binary gated with required-features (Forge-only tool)
- [25-02]: phase-server migrated from CardDatabase::load() to load_json() (last production consumer)
- [25-02]: Test assertions migrated from api_type() to effect_variant_name() (preferred over feature-gating)
- [25-03]: MIT/Apache-2.0 dual license following Rust ecosystem convention
- [25-03]: forge-compat feature gate name retained as technical identifier in documentation
- [26-02]: Direct string comparison for game passwords (no bcrypt -- appropriate for game lobby passwords)
- [26-02]: AtomicU32 for player count tracking (lock-free vs Mutex<u32>)
- [26-02]: Lobby subscribers tracked as Vec<UnboundedSender> with closed-channel cleanup
- [26-02]: password_required sentinel triggers PasswordRequired message vs generic Error
- [Phase 26]: getPlayerId() as standalone function for non-React contexts reading from Zustand getState()
- [Phase 26]: stateChanged wired in single onEvent listener rather than second subscription
- [Phase 26]: setGameState/setWaitingFor added as minimal setters to gameStore for external state updates
- [26-04]: P2PHostAdapter wraps WasmAdapter + PeerSession; host=player 0, guest=player 1
- [26-04]: filterStateForGuest hides host hand/library via JSON clone + array zeroing
- [26-04]: 5-char codes auto-detected as P2P via parseRoomCode; longer codes route to server
- [26-04]: P2P games code-only, never listed in server lobby
- [26-03]: Lobby uses its own raw WebSocket connection, separate from in-game WebSocketAdapter
- [26-03]: Timer options presented as button group (None/30s/60s/120s) rather than free-form input
- [26-03]: Password modal inline in LobbyView rather than separate route
- [26-05]: Static top-level import of @tauri-apps/plugin-shell with runtime isTauri() guard
- [26-05]: Port scanning 9374-9383 for sidecar with reuse of existing server on active port
- [26-05]: Smart detection cascade: Tauri sidecar localhost > stored server address > default
- [26-05]: parseJoinCode extracts CODE and optional server address from CODE@IP:PORT format
- [26-05]: Default server port standardized to 9374 (avoids common port conflicts)
- [26-06]: Emote auto-fade uses 3s timeout with unique numeric IDs for overlap handling
- [26-06]: Game duration tracked from gameStartedAt timestamp set on GameStarted event
- [26-06]: Back to Lobby navigates to /?view=lobby for lobby return after game over
- [28-01]: Effect::Unimplemented replaces Effect::Other -- zero HashMap, semantic marker for 2,533 unimplemented abilities
- [28-01]: ContinuousModification defined in ability.rs with layer() impl in layers.rs -- single enum for all continuous effect modifications
- [28-01]: TargetFilter uses nested And/Or/Not combinators with struct wrapper fields for serde compatibility
- [28-01]: Keywords parse cost strings through parse_keyword_mana_cost() -- supports MTGJSON brace format and simple format
- [28-01]: JsonSchema added to Zone, Phase, ManaColor, ManaCost, ManaCostShard, CoreType, Keyword for schema generation
- [28-02]: Handler function signatures in static_abilities.rs still accept HashMap -- handler rewrite deferred to Plans 03-06
- [28-02]: CmcGE computed inline from ManaCost fields since ManaCost lacks a cmc() method
- [28-02]: player_matches_filter kept as string-based -- minimal usage, not worth full typed enum
- [28-03]: SubAbility chain simplified to typed recursion with 20-depth safety limit (no SVar lookup, no parse_ability)
- [28-03]: extract_target_filter_string() and get_valid_tgts_string() bridge typed TargetFilter to string-based targeting system (temporary until targeting is fully typed)
- [28-03]: parse_cost() kept always-available (not forge-compat gated) -- used by JSON ability loading path
- [28-03]: target_filter_matches_object() in triggers.rs handles runtime TargetFilter matching for trigger validation
- [28-06]: Forge-compat gating for test modules using parse_ability() -- tests remain functional when feature enabled
- [28-06]: Effect::Unimplemented { name, description: None } for test dummy effects -- semantically clear placeholders
- [28-06]: json_smoke_test made tolerant of old-schema ability JSON files during migration period
- [28-06]: CardFace.svars removed, power/toughness use PtValue, keywords use Keyword enum (matching Plan 01 types)
- [28-04]: JSON-level migration via serde_json::Value instead of typed deserialization -- avoids old-format/new-type mismatch
- [28-04]: Unresolvable SVar Execute references logged as warnings (15,930 triggers) -- SVars not available in JSON files
- [28-04]: Migration binary runs without forge-compat feature -- operates on JSON values, not Forge parser
- [27-01]: build_resolved_from_def carries duration from AbilityDefinition to ResolvedAbility in both casting.rs and triggers.rs
- [27-01]: ExileLink tracks exiled_id/source_id as simple struct for exile-return resolution
- [27-01]: Oblivion Ring LTB trigger left without execute -- handled by ExileLink system in Plan 03
- [27-02]: Aura targeting inserted before has_targeting_requirement -- Auras target via Enchant keyword, not via Effect target field
- [27-02]: spell_targets cloned before fizzle path in resolve_top to preserve targets for Aura attachment after resolution
- [27-02]: Re-read obj after evaluate_layers in casting.rs to satisfy Rust borrow checker
- [27-03]: extract_target_filter_from_effect excludes SelfRef/Controller/None as non-targeting (no player choice needed)
- [27-03]: Multi-target triggers return early from process_triggers; remaining triggers deferred until after target selection
- [27-03]: check_exile_returns placed after SBAs and before triggers so returned permanents get ETB triggers
- [27-03]: Exile return gracefully handles cards already moved from exile (no panic, no-op)
- [27-04]: TargetingOverlay reuses existing targeting UI for TriggerTargetSelection (no separate component)
- [27-04]: Cancel button hidden for trigger targeting (MTG triggers are mandatory)
- [29-01]: CommanderDamageEntry struct instead of HashMap<(PlayerId, ObjectId), u32> for serde_json compat (tuple keys don't serialize)
- [29-01]: PartialOrd + Ord derived on PlayerId for BTreeSet<PlayerId> support
- [29-01]: priority_pass_count kept alongside new priority_passes BTreeSet (migration deferred to Plan 02)
- [29-02]: priority_passes BTreeSet tracks consecutive passes; stack resolves when set size >= living player count
- [29-02]: opponent() kept as deprecated wrapper calling players::next_player() (removed in Plan 05)
- [29-02]: SBAs collect all losers before eliminating (handles simultaneous life loss in multiplayer)
- [29-02]: 2HG team elimination cascades through teammates() — one teammate dies, both eliminated
- [29-04]: is_commander flag on GameObject for direct identification (not separate tracking structure)
- [29-04]: commander_cast_count as HashMap<ObjectId, u32> on GameState for per-commander tax tracking (partner support)
- [29-04]: Zone redirection in move_to_zone() at lowest level -- catches all zone changes including SBA-driven ones
- [29-04]: Commander tax adds to generic mana cost rather than separate payment step
- [29-06]: eval.rs uses average opponent life to avoid inflating score in multiplayer
- [29-06]: combat_ai.rs uses min opponent life for aggression heuristic (attack weakest)
- [29-06]: Session uses Vec-based player tracking with ai_seats HashSet for N-player
- [29-06]: Protocol player_count defaults to 2 via serde default for backward compat
- [29-05]: engine.rs DeclareBlockers uses WaitingFor player field instead of recomputing defending player
- [29-05]: opponent() removed from priority.rs (zero external callers in engine crate)
- [Phase 29]: No code changes needed for Plan 16 — Plan 05 already eliminated all PlayerId(1-x) from effects modules
- [Phase 29]: FormatConfig::standard() used for N-player scenario constructor (20 life default)
- [Phase 29]: mill.rs was the only effects module with hardcoded 2-player opponent logic; replaced with players::next_player()
- [29-09]: threat_level() uses weighted combination: board 40%, life 20%, hand 15%, commander damage 25%
- [29-09]: Multiplayer eval uses threat-weighted opponent scoring; 2-player retains original averaging path
- [29-09]: choose_attackers_with_targets() as primary API; choose_attackers() backward-compat wrapper
- [29-09]: Alpha-strike detection allocates smallest attackers first to just exceed lethal threshold
- [29-09]: create_config_for_players() applies player-count scaling on top of platform scaling
- [Phase 29]: PlayerArea creatureOverride prop keeps blocker sorting in GameBoard
- [Phase 29]: buildAttacks defaults all attackers to first non-eliminated opponent; per-creature target selection in Plan 08
- [Phase 29]: AttackTarget uses discriminated union { type, data } matching Rust serde output
- [29-08]: AttackTargetPicker modal with "Attack All" and "Split Attacks" modes; only shown for 2+ valid targets
- [29-08]: BlockAssignmentLines dims lines for attackers not targeting current player (opacity 0.25 vs 0.7)
- [29-08]: Engine valid_block_targets already restricts blocker assignments; no additional client-side filtering needed
- [29-10]: FORMAT_DEFAULTS as const record with Standard/Commander/FFA/2HG presets
- [29-10]: PlayerSlot model with playerId, name, isReady, isAi, aiDifficulty, deckName
- [29-10]: WaitingScreen delegates to ReadyRoom when playerSlots provided; simple mode for P2P
- [29-10]: P2P enforcement via constructor validation and validateAdapterForPlayerCount
- [29-10]: Eliminated-to-spectator auto-transition via PlayerEliminated event in WebSocketAdapter
- [29-11]: Validation warnings computed centrally in DeckBuilder, passed to DeckList (format-aware, avoids duplication)
- [29-11]: Commander stored as string[] on ParsedDeck; LegalityBadge reads Scryfall legalities directly
- [29-11]: MTGJSON legalities HashMap<String, String> with serde(default) for backward compat
- [Phase 29]: LobbyView accepts connectionMode prop to show mode-specific UI (server vs P2P)
- [30-01]: GameEvent derives Eq (required by StackEntryKind Eq derive with trigger_event field)
- [30-01]: Event-context TargetFilter variants excluded from extract_target_filter_from_effect (auto-resolve, no targeting)
- [30-01]: TriggeringSource used for both "that source" and "that permanent" in trigger context
- [30-01]: Effect::AddRestriction handler is no-op stub pending Plan 02 wiring
- [30-02]: PendingTrigger carries trigger_event to preserve matched event through APNAP ordering and stack push
- [30-02]: Prevention gating uses description-based classification rather than new field on ReplacementDefinition
- [30-02]: "damage can't be prevented" rerouted from replacement to effect parser as Effect::AddRestriction
- [30-02]: fill_source() pattern fills placeholder ObjectId(0) from parser with actual source at resolution time

### Roadmap Evolution

- Phase 26 added: Polish and fix multi-player with lobby and embedded server
- Phase 28 added: Native Ability Data Model — Eliminate all remaining Forge scripting DSL from card data format and engine runtime
- Phase 29 added: Support N players in engine and on board in React for various formats
- Phase 30 added: Implement the required building blocks specified in the summary
- Phase 31 added: Add mechanics to support Kaito, Bane of Nightmares (docs/plan-kaito-bane-of-nightmares.md)

### Pending Todos

None yet.

### Blockers/Concerns

- ~~GPL contamination legal analysis has LOW confidence~~ — Resolved: Forge data deleted, parser feature-gated, project relicensed MIT/Apache-2.0

## Session Continuity

Last session: 2026-03-17T00:39:57Z
Stopped at: Completed 30-02-PLAN.md
Resume file: .planning/phases/31-add-mechanics-to-support-kaito-bane-of-nightmares-docs-plan-kaito-bane-of-nightmares-md/31-CONTEXT.md
