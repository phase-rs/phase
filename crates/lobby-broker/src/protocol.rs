//! Wire protocol for the lobby/matchmaking broker.
//!
//! This is the **lobby subset** of `server_core::protocol`, wire-compatible by
//! construction: every type uses the same `#[serde(tag = "type", content =
//! "data")]` shape and identical field names/`#[serde(default)]` attributes as
//! the canonical `ClientMessage`/`ServerMessage`, so the bytes on the wire are
//! byte-identical regardless of which enum (de)serializes a given frame.
//!
//! `LobbyGame` and `DraftLobbyMetadata` are **defined here** (the broker owns
//! the lobby-listing wire types) and re-exported by `server_core::protocol`, so
//! `server_core::ServerMessage::LobbyUpdate { games: Vec<LobbyGame> }` and the
//! broker both reference the same struct.
//!
//! Incoming frames are deserialized via a **two-stage parse** (`Envelope` →
//! tag match → variant): `#[serde(other)]` is invalid on adjacently-tagged
//! enums, so an unrecognized `type` is routed to the reject path explicitly
//! rather than collapsing into a magic catch-all variant (plan decision A2).

use engine::starter_decks::DeckData;
use engine::types::format::{FormatConfig, GameFormat};
use engine::types::match_config::MatchConfig;
use serde::{Deserialize, Serialize};

/// Machine-readable reasons for server error replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerErrorCode {
    DeckRejected,
}

/// Wire-protocol version shared by the native server, client, and Cloudflare
/// lobby Worker. Bump when any `ClientMessage` or `ServerMessage` variant is
/// added, removed, renamed, or has a field type changed. Adding a new optional
/// field with `#[serde(default)]` does not require a bump — **unless the client
/// stops deriving a fallback for it.** That clause is about wire
/// *parseability*, not capability: once the client renders a feature only from
/// the new field, an old server that omits it produces a silent feature loss
/// rather than a parse error, and the handshake is the only place that pairing
/// can be refused. See 24.
///
/// 63 — `WaitingFor::ReplacementChoice` gained an engine-owned
///      `ReplacementChoiceKind` discriminator and a `last_applied_decides`
///      flag. Both are `#[serde(default)]`, so a v62 peer decodes the payload
///      successfully and then falls back to the `Order` default: it renders a
///      drag-to-order list for a yes/no "you may" prompt, and names a winning
///      outcome for a compositional collision that has none. A silent
///      misrender rather than a decode failure, so the exact-match full-game
///      handshake must refuse the pairing. Lobby messages are unchanged.
/// 62 — `ServerMessage::{GameStarted, StateUpdate}` gained
///      `activation_block_reasons: HashMap<ObjectId, Vec<AbilityBlockEntry>>` —
///      the CR 118.3 "you can't pay this cost right now" read-out, scoped to the
///      acting player and empty for everyone else. It carries
///      `#[serde(default, skip_serializing_if = "HashMap::is_empty")]`, so a v61
///      payload reads as the absent/empty map it always meant. The break is the
///      other direction, and it is over-determined: `AbilityBlockKind` is
///      `#[serde(tag = "type")]`, so the new `CostNotPayableNow` arm emits a TAG
///      VALUE no v61 peer has a case for — a v61 Rust peer fails to deserialize
///      the entry, and a v61 client indexes its reason-key record with an
///      unknown member and renders `t(undefined)`. New tag, not merely a new
///      field, so the bump does not rest on the serde attributes above.
///
/// 61 — `Effect::ChooseCounterKind` gained `domain: CounterKindDomain` and
///      `chooser: CounterKindChooser` — the population a counter-kind choice
///      draws its legal kinds from, and whether the GAME draws one at random
///      instead of prompting the controller (CR 608.2d). Both carry
///      `#[serde(default)]`, so a v60 payload reads as the on-target/controller
///      form it always meant. The break is the other direction: a v60 peer has
///      no field to receive `Printed`/`Random` into and sets no
///      `deny_unknown_fields` to reject them, so it reads Crystalline Giant's
///      printed-list random draw as an on-target choice, finds no counters on a
///      fresh Giant, and places nothing — the exact defect this bump ships the
///      fix for (#7796). Abilities and trigger definitions ride inside
///      `GameObject`, so every full-GameState frame carries the shape. Full-game
///      floors are exact-match on both sides, so the pairing is refused at the
///      handshake. Lobby messages are unchanged.
/// 60 — `DerivedViews::back_face_spell_costs` publishes, for each card the
///      viewer may cast whose player chooses a spell face at cast time (a split
///      card such as a Room, a spell//spell MDFC — CR 709.3 + CR 712.11b), the
///      live cost of the OTHER face; `spell_costs` reports the live face only.
///      The cost badge renders both faces from this map. Serde-additive, but
///      the client renders the map directly; a v59 host would silently show a
///      Room's single-face badge again, on top of the second half's printed
///      cost. Full-game handshakes must refuse that capability mismatch. Lobby
///      messages are unchanged.
/// 59 — `InteractionResponseSpec::Shortcut::preview` changed from
///      `Option<InteractionShortcutPreview>` to `Vec<InteractionShortcutPreview>`,
///      one element per offerable count, and each element gained
///      `allocation: Vec<AmountAssignment>`, the declaration's shape over that
///      element's count. The retype is the break; `allocation` is not — it carries
///      `#[serde(default, skip_serializing_if = "Vec::is_empty")]`, neither type
///      sets `deny_unknown_fields`, and it parses in both directions. A PARSE bump
///      like 23, 36 and 42, not a capability bump like 24 — and an ASYMMETRIC one,
///      so both directions are stated. v58 → v59 fails on EVERY shortcut offer: the
///      old field carried no `skip_serializing_if`, so a v58 peer always emits the
///      key, and neither `null` nor an object deserializes into a sequence.
///      v59 → v58 fails only on an offer that actually carries a preview, because
///      an empty list omits the key and a v58 peer's `Option` field reads that as
///      `None`. The retype breaks the declared Rust types, but no production Rust
///      code deserializes `ServerMessage` — the browser half is what decodes those
///      frames, with `JSON.parse` and no validation, which is why the handshake is
///      the only place the pairing is refusable. No shim ships
///      — no `deserialize_with`, no dual-parse path, no version-conditional branch —
///      and full-game floors are exact-match on both sides
///      (`server_core::MIN_SUPPORTED_PROTOCOL == PROTOCOL_VERSION`, and
///      `MIN_SUPPORTED_SERVER_PROTOCOL` in `client/src/adapter/ws-adapter.ts`), so
///      the pair is refused before it sends the frame. Lobby messages are unchanged.
/// 58 — `DraftPlayerView::commanders_required` publishes the procedure-owned
///      commander designation count. The client renders designation controls
///      from this required field rather than inferring them from `DraftKind`;
///      older full servers omit it. Lobby messages are unchanged.
/// 57 — `GameAction::BeginResolveAll` gained `scope: ResolveAllScope` (`Own`
///      binds only the requester and resolves immediately; `Shared` opens the
///      table-wide consent protocol), and `PriorityPassingMode` gained
///      `FullControl`, which is now engine-authoritative rather than a
///      frontend-only toggle.
/// 56 — Host-only authoritative-state export request/response variants. Native
///      P2P host diagnostics must use a trusted server envelope rather than a
///      redacted player view. Lobby messages are unchanged.
/// 55 — `DerivedViews::room_half_identities` publishes both halves of every
///      battlefield Room in printed order, resolved through the COPIED halves
///      for a permanent that copies a Room (CR 709.5b + CR 707.2). The unlock
///      special action's offer names the half and shows its unlock cost
///      (CR 709.5e) from this map; an enter-as-copy recipient carries neither
///      on its own printed card, and printed order is engine work. The field
///      is serde-additive, but the client renders the map directly rather than
///      deriving halves from raw state; a v54 host would silently label every
///      offered door "Tap for Mana" again. Full-game handshakes must refuse
///      that capability mismatch. Lobby messages are unchanged.
/// 54 — `CreateDraftWithSettings` now carries a tagged `DraftSourceIntent`.
///      A Chaos client sends only candidate set codes; the Full server resolves
///      and persists the private seat-by-round assignment matrix. This changes
///      a Full-server client message, so the full handshake refuses stale
///      peers. Lobby messages are unchanged.
/// 53 — `DraftPlayerView::launch_capability` publishes the engine-authorized
///      post-draft multiplayer launch. The client renders this procedure-owned
///      capability instead of inferring it from `DraftKind`; an older server
///      would omit it and silently hide a completed Commander pod's launch.
///      Full-game handshakes must refuse that capability mismatch. Lobby
///      messages are unchanged.
/// 52 — `DerivedViews::storm_count` publishes the engine-owned number of
///      copies a current Storm trigger will create, or a newly cast Storm spell
///      would create. The field is serde-additive, but the client renders this
///      scalar directly rather than deriving Storm from raw state; a v51 host
///      would therefore silently omit the HUD status. Full-game handshakes
///      must refuse that capability mismatch. Lobby messages are unchanged.
/// Note: renaming or removing a variant silently fails at JSON parse time
/// (clients see "Invalid message: unknown variant") rather than at the
/// handshake. When making such changes, plan a deprecation window where
/// both the old and new variants coexist, then bump and remove the old.
///
/// 51 — Casting permissions gained a typed lifetime. Two parts:
///      (a) `CastingPermission::ExileWithAltAbilityCost` gained `duration` and
///      `source_id`; `::ExileWithAltCost` gained `source_id` beside the
///      `duration` it already carried. `source_id` is the granting permanent
///      that bounds a host-lifetime duration — additive behind serde defaults.
///      (b) `Duration` gained the `WhileControllingHost` ("for as long as you
///      control ~") and `WhileHostOnBattlefield` ("for as long as ~ remains on
///      the battlefield") variants (CR 611.2b). Each new tag is a ONE-WAY
///      parse break like entry 46: a v50 peer cannot deserialize a `GameState`
///      containing it, and no serde default can rescue an unknown variant, so
///      the full-game handshake must refuse the pairing. Lobby messages are
///      unchanged.
/// 50 — `FormatConfig` gained `default_deck_copy_limit`, the resolved
///      per-format deck-copy ceiling (CR 100.2a / CR 100.2b / CR 903.5b)
///      `max_deck_copies` and the deck-compatibility admission path now both
///      read, replacing per-function hardcoded literals and bare-`GameFormat`
///      -derived defaults so the two authorities can't disagree. A CAPABILITY
///      bump like 24: the field is `#[serde(default =
///      "default_deck_copy_limit_fallback")]` (`UpTo(1)`, the tightest
///      possible cap), so a peer missing it still deserializes `GameState`
///      cleanly — but silently loses the format's real declared limit and
///      falls back to the fail-closed singleton cap, wrongly rejecting a
///      legal 4-of deck under Standard/Pioneer/etc. rather than admitting one
///      it shouldn't. The direction is symmetric: whichever peer lacks the
///      field degrades the same way, fail-closed, never fail-open. Lobby
///      carriers move too; see `LOBBY_PROTOCOL_VERSION` 3.
/// 49 — Full-server `DraftPlayerView` payloads require public-seat
///      `active_pack_count`. An older v48 server can complete the handshake
///      yet omit that serde-additive field while the TypeScript client accepts
///      the JSON, leaving it unable to render a seat's active-pack presence.
///      Full handshakes must refuse the capability mismatch. Lobby messages
///      are unchanged.
/// 48 — Full-server `DraftPlayerView` payloads require the engine-owned
///      `pick_selection_mode`. An older server can omit it while the
///      TypeScript client accepts the JSON, then silently treats an ordered
///      Commander Draft pick as direct selection. Lobby messages are unchanged.
/// 47 — Resolution-time optional fixed sacrifice payments add a typed
///      replacement-resumable continuation to `GameState`.
/// 46 — `QuantityRef::Aggregate` and `QuantityRef::TrackedSetAggregate` were
///      replaced on the serialized `GameState` surface by the canonical
///      `QuantityRef::PropertyAggregate` tag with a validated `source` object.
///      New peers accept the two legacy input tags, but a v45 peer cannot
///      deserialize the canonical tag emitted by v46, so full-game handshakes
///      must refuse the one-way parse mismatch. Lobby messages are unchanged.
/// 45 — `GameState` gained serialized cast-occurrence provenance and prepared-copy links.
/// 44 — Resolution-time optional `PayCost(OneOf)` branch choice added a
///      serialized `WaitingFor`/`GameAction` pair.
/// 43 — Engine-owned stack-resolution automation retired the legacy native
///      Resolve All request/result wire messages.
/// 42 — `FormatConfig.deck_size` changed from a bare `u16` to the adjacently
///      tagged `DeckSizeRule` enum (`Minimum(u16)` / `Exactly(u16)`), because
///      CR 903.13f(1) makes Commander Draft a command-zone format with a
///      minimum rather than an exact size, and `GameFormat` gained a
///      `CommanderDraft` variant (CR 903.13a). A PARSE bump like 23 and 36,
///      not a capability bump like 24: `FormatConfig::deck_size` carries
///      neither `#[serde(default)]` nor `deserialize_with`, so a v41 peer's
///      `"deck_size": 60` fails against the adjacently tagged enum and a v42
///      peer's `{"type":"Minimum","data":60}` fails against a v41 `u16` — the
///      break is unconditional and runs in BOTH directions, for every format.
///      `GameState::format_config`'s `#[serde(default = "FormatConfig::standard")]`
///      does NOT rescue it: a field-level default applies only when the key is
///      ABSENT, and an old peer sends the key present with the old inner shape,
///      so the default never runs. The `GameFormat::CommanderDraft` variant is
///      the second and narrower half — it breaks only when that variant is
///      actually serialized.
/// 41 — Operational failure responses are correlated to their pending action.
/// 40 — Action rejection responses carry engine-owned structured context.
/// 39 — `ManaRestriction::CannotCastSpellFromZone` adds a serialized
///      GameState/ManaUnit restriction used by Karolina Dean. Older peers
///      cannot deserialize that externally tagged enum variant. Lobby messages
///      are unchanged.
/// 38 — `WaitingFor::ChooseObjectsSelection` publishes the resolving effect's
///      `min` and optional `max` bounds. Older clients parse and silently
///      ignore these additive fields, then offer selections outside the
///      engine-authoritative range; refuse that capability mismatch at the
///      full-game handshake. Lobby messages are unchanged.
/// 37 — `PayCostKind::TapCreatures` changed from `{ aggregate:
///      Option<TapCreaturesAggregate> }` to a required `{ mode:
///      TapCreaturesSelectionMode }` (Fixed/VariableX/Aggregate) — the fix
///      that also unlocks the `u32::MAX` X-sentinel tap-cost form (Glacian,
///      Powerstone Engineer + 8 sibling cards, #7799). `mode` carries no
///      `#[serde(default)]`: a `GameState` snapshot paused mid-TapCreatures
///      payment (Crew/Saddle/Teamwork/Conspire, or the newly-unlocked
///      X-sentinel form) under the old `aggregate` shape now fails
///      deserialization rather than risk silently misclassifying an
///      aggregate payment as fixed-count (or vice versa) — exactly the
///      ambiguity `TapCreaturesSelectionMode` exists to make
///      unrepresentable. Old and new peers can't parse each other's
///      serialized snapshots while such a payment is in flight.
/// 36 — `WaitingFor::ChooseDungeon::options` changed from `Vec<DungeonId>` to
///      `Vec<DungeonPreview>`, and `WaitingFor::ChooseDungeonRoom` dropped
///      `option_names`, gained a required `dungeon_name`, and changed `options`
///      from `Vec<u8>` to `Vec<RoomPreview>`, so each option carries the room's
///      printed name and room-ability text (CR 309.4b-c). A PARSE bump like 23,
///      not a capability bump like 24: none of the new fields carry
///      `#[serde(default)]`, so a v35 peer fails deserialization on a
///      dungeon-choice `GameState` outright rather than degrading silently.
///      `DerivedViews::dungeon_rooms` rides along in the same bump — it IS
///      `#[serde(default)]`, but the client deleted its `dungeon_progress`
///      room-index derivation, so a v35 server that omits it would leave a new
///      client rendering no dungeon badge at all.
/// 35 — `DerivedViews::current_target_kind` publishes the engine's CR 115.1
///      classification of the live target announcement. A CAPABILITY bump like
///      24 and 32, not a parse bump: the field is `Option` +
///      `skip_serializing_if`, but the client deleted `inferTargetNoun`, so a
///      v34 server that omits it would leave a new client naming no target at
///      all — silently, with no parse error to catch it.
/// 34 — `DraftKind::CommanderDraft` (CR 903.13a) is serialized by draft
///      WebSocket messages, and `DraftAction::Pick` renamed
///      `card_instance_id: String` to `card_instance_ids: Vec<String>` to carry
///      a whole CR 903.13b pick step. The rename is a PARSE bump, not a
///      capability bump: `card_instance_ids` carries no `#[serde(default)]`, so
///      a v33 `Pick` frame fails deserialization on a v34 peer and vice versa.
///      `DraftAction::SubmitDeck::commanders` is additive and `#[serde(default)]`
///      — exempt on its own; it is listed because 34 carries it, not because it
///      forces 34.
///      For a client that advertises `lobby_protocol_version`, this number
///      does not gate lobby admission: both ends compare the LOBBY number
///      instead, and the client echoes the broker's own `protocol_version`
///      back (`openPhaseSocket.ts`) so the legacy window compares a value to
///      itself. On the LEGACY path it DOES gate, at both ends — the
///      `MIN_SUPPORTED_PROTOCOL ..= PROTOCOL_VERSION` window here, and a
///      client-side ceiling in `ws-adapter.ts` that runs BEFORE that echo —
///      so a client built in the window between the previous bump and the
///      lobby-owned version is evicted by this move. No RELEASED tag sits on
///      the legacy path with a `protocol_version` inside the post-bump
///      window; every released build still on that path is already below its
///      own ceiling and stays refused. Lobby gating lives in
///      `LOBBY_PROTOCOL_VERSION` / `MIN_SUPPORTED_LOBBY_PROTOCOL`, which
///      move to 2 alongside this bump for the `FormatConfig.deck_size`
///      retype — that pair is what evicts a released cohort; see its own
///      changelog entry for who and for what is lost.
/// 33 — `LegendCandidateIdentity::Unknown` prevents face-down legend candidates
///      from publishing an affirmative original/copy identity.
/// 32 — `DerivedViews::legend_candidate_identities` publishes the engine-authored
///      original/copy/token-copy identity for each active legend-rule choice. The
///      field is `#[serde(default)]`, but the client deliberately no longer derives
///      this rules-sensitive identity from raw objects; an older server would
///      silently omit every choice identity.
/// 31 — `WaitingFor::LoopShortcut` publishes the engine-issued `declaration`, and
///      `InteractionResponseSpec::Shortcut` publishes `preview`, the per-axis
///      consequence of the offered count. Both are `Option` and neither type sets
///      `deny_unknown_fields`, so a v30 peer still *parses* the frame — this is a
///      capability bump like 24, not a parse bump. UNLIKE 24, no pairing is left to
///      exercise the gap, so this entry names no silent-drop hazard. Full-game floors
///      are exact-match on both sides (`server_core::MIN_SUPPORTED_PROTOCOL ==
///      PROTOCOL_VERSION`, and `MIN_SUPPORTED_SERVER_PROTOCOL` in
///      `client/src/adapter/ws-adapter.ts`), so a v31/v30 full-game pair is refused
///      at the handshake and never sends an action frame. The one-version window is
///      this file's `MIN_SUPPORTED_PROTOCOL` below, and it is lobby-only:
///      `DeclareShortcut` rides `ClientMessage::Action`, which `LobbyClientMessage`
///      has no variant for at all, and which `reject_if_disabled` in
///      `crates/phase-server/src/main.rs` answers under `ServerMode::LobbyOnly` with
///      an explicit rejection rather than a silent drop. The P2P games this broker
///      matchmakes are gated tighter still, on build-commit equality
///      (`check_build_commit`), not on a protocol window.
/// 30 — Serialized player-action completion provenance and modal continuations.
/// 29 — Added requester-correlated `ResolveAllRejected` response frames.
/// 28 — Added native `ResolveAll` request/result frames.
/// 27 — Added `DraftKind::Sealed`, serialized by draft WebSocket messages.
/// 26 — Added `ServerMessage::ActionNoOp` for accepted transport no-ops.
/// 25 — `DebugCardEntries` added a serialized, private resolution frame for
///      multi-card sandbox battlefield entries that pause for replacement or
///      as-enters choices. Old peers cannot deserialize that `GameState` shape.
/// 24 — `DerivedViews::unbounded_families` carries the engine-owned per-seat
///      family collapse state behind each `∞` badge. The field is
///      `#[serde(default)]`, so this is a capability bump rather than a parse
///      bump: the client deleted its row-flag OR-fold derivation, so a v23
///      server that omits the field would leave a new client rendering NO
///      infinity badges at all, silently and with no parse error to catch it.
/// 23 — `PayableResource::ManaGeneric` changed from `{ per_x }` to
///      `{ base_cost: ManaCost }` (#6410) — a `GameState` payload field type
///      change, and `base_cost` intentionally carries no `#[serde(default)]`
///      (a missing `base_cost` must fail deserialization, not silently
///      resolve to a zero-cost payment), so old and new peers can't parse
///      each other's serialized snapshots.
/// 20 — Actor-scoped priority-passing settings and filtered per-player state.
/// 19 — Connive's exact `EventObjectSnapshot` subject and resident paused
///      post-replacement drains changed serialized full-game state. Phase 4
///      later pinned the existing v2 resolution wire shape; it did not add a
///      second protocol change.
/// 18 — Serialized GameState trigger provenance and paused logical zone-change
///      owners are now wire-visible.
/// 16 — Meld pair/attacking-entry choices after mana-payment preview variants.
/// 15 — Mana-payment preview request/response variants.
/// 14 — `PrecastCopyShortcut` action and its two `WaitingFor` variants.
/// 13 — `WaitingFor::MulliganBottomCards` removed from the full-game state
///      payload; mulligan bottoming folded into a
///      `MulliganDecisionPhase::BottomCards` sub-phase on
///      `WaitingFor::MulliganDecision`.
pub const PROTOCOL_VERSION: u32 = 63;

/// Minimum protocol version accepted by lobby-only brokers at the hello
/// handshake **from clients that predate [`LOBBY_PROTOCOL_VERSION`]** — the
/// legacy path only. Lobby traffic has a one-version rollout window; full game
/// servers may choose a stricter floor when state/action payloads change.
///
/// Being derived from [`PROTOCOL_VERSION`] is exactly the defect
/// [`LOBBY_PROTOCOL_VERSION`] fixes: a `GameState`-only bump slides this floor
/// even though no lobby message changed. It survives only so already-deployed
/// clients stay reachable; new gating must use [`MIN_SUPPORTED_LOBBY_PROTOCOL`].
pub const MIN_SUPPORTED_PROTOCOL: u32 = PROTOCOL_VERSION.saturating_sub(1);

/// Wire-protocol version of the **lobby** message set ([`LobbyClientMessage`] /
/// [`LobbyServerMessage`]), independent of [`PROTOCOL_VERSION`].
///
/// Bump ONLY when a lobby variant is added, removed, renamed, or has a field
/// type changed. A full-game bump must NOT move this number: no lobby variant
/// carries `GameState` or `GameAction`, so full-game churn cannot break lobby
/// traffic.
///
/// Sharing one integer between the two surfaces is what took preview
/// multiplayer down: `PROTOCOL_VERSION` moved twice for `GameState`-only
/// changes, [`MIN_SUPPORTED_PROTOCOL`] is derived from it, and the deployed
/// broker's window went disjoint from the shipped client's. This constant is
/// the fix — it moves only for reasons the lobby can actually observe.
///
/// 4 — The tournament-organizer message set: seven [`LobbyClientMessage`]
///     variants (`CreateTournament`, `JoinTournament`, `GetTournament`,
///     `StartTournamentRound`, `ReportMatchResult`, `DropFromTournament`,
///     `EndTournament`) and five [`LobbyServerMessage`] variants
///     (`TournamentCreated`, `TournamentJoined`, `TournamentUpdate`,
///     `TournamentRemoved`, `TournamentListUpdate`). "A lobby variant is
///     added" is the first of the four triggers listed above, so this bump is
///     required by this constant's own documented policy.
///
///     Purely ADDITIVE, which is why [`MIN_SUPPORTED_LOBBY_PROTOCOL`] does
///     **not** move alongside it — the asymmetry with 2 is deliberate. Bump 2
///     retyped a field three existing carriers already held in both
///     directions, so a v1 peer could neither send nor interpret a frame it
///     routinely exchanged; only a floor move could say that out loud. Nothing
///     here changes any existing variant's shape. A v2 client keeps parsing
///     every frame it already understood, never sends a tournament variant,
///     and — for the broker → client half where `JSON.parse` validates nothing
///     — receives no tournament frame at all unless it first subscribed to a
///     surface it has no code for. Raising the floor would evict that entire
///     cohort's lobby session to protect it from messages it cannot receive.
///     On the client → broker half a v4-only tag reaching a v2 broker is
///     already answered per-frame by [`ParsedFrame::UnknownTag`], which is the
///     stated reason no upper bound exists either.
/// 3 — `FormatConfig` gained `default_deck_copy_limit` (see `PROTOCOL_VERSION`
///     50 for the full entry). Same three carriers as 2:
///     `CreateGameWithSettings` on [`LobbyClientMessage`] (client → broker),
///     `JoinTargetInfo` and `PeerInfo` on [`LobbyServerMessage`] (broker →
///     client). Unlike 2, this is a CAPABILITY bump, not a parse bump — the
///     field is `#[serde(default)]` and still deserializes cleanly on either
///     side — so [`MIN_SUPPORTED_LOBBY_PROTOCOL`] does NOT move: a v2 client
///     can still create/join a game, it just can't declare or observe a
///     non-default deck-copy-limit override, silently getting the fail-closed
///     `UpTo(1)` fallback instead of the format's real default. That is a
///     capability loss, not a broken session — the same shape as
///     `PROTOCOL_VERSION`'s own capability-bump entries (24, 50), not this
///     file's own entry 2.
/// 2 — `FormatConfig::deck_size` changed from a bare `u16` to the adjacently
///     tagged `DeckSizeRule` — a field TYPE change, one of the four triggers
///     listed above.
///
///     Three lobby carriers hold a `FormatConfig`, in both directions:
///     `CreateGameWithSettings` on [`LobbyClientMessage`] (client → broker),
///     and `JoinTargetInfo` and `PeerInfo` on [`LobbyServerMessage`]
///     (broker → client). The client → broker one fails LOUDLY: serde refuses
///     a bare integer for an adjacently tagged enum, so the frame is reported
///     as [`ParsedFrame::Malformed`]. The two broker → client carriers cannot
///     fail at all — the browser deserializes them with `JSON.parse`, which
///     validates nothing, and the TypeScript declaration is erased at
///     runtime. That asymmetry is WHY a version number is the only available
///     signal here: on the broker → client half no per-frame check exists to
///     reject a stale shape.
///
///     What the paired floor move evicts: the entire lobby session of every
///     client built against lobby version 1 — hosting, browsing and joining
///     alike — even though exactly one `LobbyClientMessage` variant actually
///     carries a `FormatConfig` and the rest would still have parsed. It
///     migrates nothing and does not make such a client work; it converts a
///     mid-session create-game failure — and, on the join path, a `deck_size`
///     in a shape that build cannot interpret and that no per-frame check on
///     that direction can reject — into one legible handshake refusal.
/// 1 — Initial lobby-owned version, covering the `LobbyClientMessage` /
///     `LobbyServerMessage` variant sets, unchanged since #1880.
pub const LOBBY_PROTOCOL_VERSION: u32 = 4;

/// Lowest [`LOBBY_PROTOCOL_VERSION`] a broker accepts from a client.
///
/// There is deliberately **no upper bound**. A client newer than the broker can
/// only fail by sending a lobby variant this broker does not know, and
/// [`parse_lobby_client_message`] already answers that with an explicit
/// [`ParsedFrame::UnknownTag`] rejection scoped to the offending frame. That is
/// a loud, per-feature failure; refusing the whole connection instead evicts a
/// client over a variant it may never send.
///
/// The LOWER bound exists for the opposite reason, and the cohort the sentence
/// above names is exactly the one it evicts. A client that only browses and
/// joins never sends `CreateGameWithSettings`, and every other
/// [`LobbyClientMessage`] variant it uses still parses — so on the client →
/// broker direction there is indeed little to refuse it over. But every
/// `JoinTargetInfo` and `PeerInfo` that cohort RECEIVES carries the same
/// [`FormatConfig`], on the direction where the deserializer is the browser's
/// `JSON.parse`. It is handed a `deck_size` whose shape its build cannot
/// interpret, nothing in that build reads the field, and a stale shape cannot
/// fail there — no per-frame rejection is possible on that half at all. That
/// absence of any other voice is the ground for the floor: the handshake is
/// the only place the mismatch can be said out loud. The upper bound stays
/// absent because [`ParsedFrame::UnknownTag`] is available there; the lower
/// bound exists because on the broker → client half nothing equivalent is.
pub const MIN_SUPPORTED_LOBBY_PROTOCOL: u32 = 2;

/// Public-lobby view of a single registered game. Populated by the server,
/// never by clients. Field shape mirrors the pre-extraction
/// `server_core::protocol::LobbyGame` exactly for wire compatibility.
/// `PartialEq` is additive over the pre-extraction type — needed so the broker's
/// `Outbound`/`LobbyServerMessage` can derive it for order-sequence assertions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LobbyGame {
    pub game_code: String,
    pub host_name: String,
    pub created_at: u64,
    pub has_password: bool,
    /// Display string (e.g. `"0.1.11"`). Human-readable; not a compatibility gate.
    #[serde(default)]
    pub host_version: String,
    /// Git short-hash of the host's build. The compatibility gate — clients on
    /// a different commit cannot join because GameState / rules may have diverged.
    #[serde(default)]
    pub host_build_commit: String,
    /// Number of seats currently occupied (host + joined guests, including AI
    /// if present). Updated as players join/leave.
    #[serde(default)]
    pub current_players: u32,
    /// Configured seat count for this game. For 1v1 formats this is 2; for
    /// Commander it ranges 2–4.
    #[serde(default)]
    pub max_players: u32,
    /// Game format (Standard, Commander, etc.) — lets lobby UIs filter or
    /// badge the row. Optional because older persisted entries predate the
    /// field.
    #[serde(default)]
    pub format: Option<GameFormat>,
    /// Optional per-match label distinct from the host's player name. When
    /// set, lobby UIs render this as the row's primary title and the host's
    /// name as secondary metadata. `None` means "use the host's name".
    #[serde(default)]
    pub room_name: Option<String>,
    /// True when this room is P2P-brokered (host runs the engine). False for
    /// server-run rooms. Derived from `host_peer_id` presence at publish time.
    #[serde(default)]
    pub is_p2p: bool,
    /// True when the host enabled Sandbox mode. Populated from
    /// `format_config.allow_debug_actions`.
    #[serde(default)]
    pub is_sandbox: bool,
    /// True when the room is configured as ranked.
    #[serde(default)]
    pub is_ranked: bool,
    /// When present, this lobby entry is a draft pod rather than a
    /// constructed-play room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_metadata: Option<DraftLobbyMetadata>,
}

/// Metadata attached to a lobby entry when the room is a draft pod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftLobbyMetadata {
    /// Three-letter set code (e.g. "MKM", "OTJ"). For cube drafts, set to
    /// `"custom-cube"`; see [`DraftLobbyMetadata::cube_name`] for the
    /// human-readable cube name.
    pub set_code: String,
    /// Draft kind label: "Quick", "Premier", "Traditional", "Sealed", or
    /// "CommanderDraft". The field is a `String`, so adding a label is
    /// documentation only — the wire shape is transparent to it and no
    /// deserialization changes.
    pub draft_kind: String,
    /// Human-readable cube name when the pod is a cube draft. Absent for
    /// set drafts. Backward-compatible: `#[serde(default)]` accepts
    /// existing serialized records without the field; `skip_serializing_if`
    /// keeps the wire output byte-identical for set drafts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cube_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Tournament wire views
// ---------------------------------------------------------------------------
//
// Projections of `crate::tournament`'s durable types onto the wire, populated
// by the server and never by clients — the same framing [`LobbyGame`] carries.
// They exist for exactly one reason: `TournamentMeta` and `TournamentPlayer`
// hold `organizer_token`/`player_token`, and those two secrets must never
// reach a client that does not already own them. Serializing the domain types
// directly would leak every player's token to every subscriber, so the token
// fields are dropped by construction here rather than by a
// `skip_serializing_if` a later edit could quietly remove.
//
// Domain types that carry no secret (`MatchArity`, `BracketShape`,
// `TournamentStatus`, `PairingOutcome`, `TournamentStanding`) are reused
// directly as field types rather than mirrored, so a change to one of them
// cannot leave a parallel wire copy behind.

/// One entrant, as any client may see them. The token-free half of
/// [`crate::tournament::TournamentPlayer`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerSummary {
    pub player_key: String,
    pub display_name: String,
    pub dropped: bool,
}

impl From<&crate::tournament::TournamentPlayer> for PlayerSummary {
    fn from(player: &crate::tournament::TournamentPlayer) -> Self {
        Self {
            player_key: player.player_key.clone(),
            display_name: player.display_name.clone(),
            dropped: player.dropped,
        }
    }
}

/// One pairing, with its seats resolved from bare `player_key`s to full
/// [`PlayerSummary`]s so a client can render it without a second lookup.
///
/// `players` is a `Vec`, not a `player_a`/`player_b` pair: the same shape
/// carries a head-to-head pairing, a full or short pod at any
/// [`crate::tournament::MatchArity`], and a one-seat bye.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingView {
    pub id: crate::tournament::PairingId,
    pub round: u32,
    pub players: Vec<PlayerSummary>,
    /// `None` while the pairing is still pending.
    pub outcome: Option<crate::tournament::PairingOutcome>,
}

/// One row of the tournament list — enough to render a lobby listing without
/// fetching the full [`TournamentView`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TournamentSummary {
    pub code: String,
    pub name: String,
    pub arity: crate::tournament::MatchArity,
    pub bracket: crate::tournament::BracketShape,
    pub status: crate::tournament::TournamentStatus,
    /// **Active** entrants — `TournamentMeta::active_player_count`, not
    /// `players.len()`. A listing answers "how many players are still in this
    /// event", which is what a browsing client is deciding on; a dropped
    /// player is not one of them. [`TournamentView::players`] still carries
    /// every registered entrant, dropped ones included, so the detail view
    /// never loses history this row summarizes away.
    pub player_count: u32,
    pub current_round: u32,
    /// The scheduled length, read through
    /// [`crate::tournament::TournamentMeta::total_rounds`] — the single
    /// authority that resolves the organizer override, the latched default,
    /// and the live default in that order.
    pub total_rounds: u32,
    pub created_at: u64,
}

impl From<&crate::tournament::TournamentMeta> for TournamentSummary {
    fn from(meta: &crate::tournament::TournamentMeta) -> Self {
        Self {
            code: meta.code.clone(),
            name: meta.name.clone(),
            arity: meta.arity,
            bracket: meta.bracket,
            status: meta.status,
            player_count: meta.active_player_count(),
            current_round: meta.current_round,
            total_rounds: meta.total_rounds(),
            created_at: meta.created_at,
        }
    }
}

/// The full detail view of one tournament: its summary row plus every
/// registered entrant, the complete pairing history, and the standings
/// recomputed fresh from that history.
///
/// `players`/`pairings` map 1:1 from
/// [`crate::tournament::TournamentMeta::players`]/`.pairings` — a full view,
/// never a filtered subset. Dropped players stay listed (their `dropped` flag
/// is the distinction a client renders), and every round's pairings stay
/// present, because the standings are only interpretable against the history
/// that produced them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TournamentView {
    pub summary: TournamentSummary,
    pub players: Vec<PlayerSummary>,
    pub pairings: Vec<PairingView>,
    pub standings: Vec<crate::tournament::TournamentStanding>,
}

impl From<&crate::tournament::TournamentMeta> for TournamentView {
    fn from(meta: &crate::tournament::TournamentMeta) -> Self {
        let player_summary = |key: &String| {
            meta.player(key)
                .map(PlayerSummary::from)
                .unwrap_or_else(|| {
                    // A pairing seat naming a player the field does not hold is
                    // unreachable through `TournamentManager` (pairings are built
                    // from the standings order, itself derived from `players`).
                    // Rendering the bare key rather than dropping the seat keeps a
                    // corrupted snapshot legible instead of silently shrinking a
                    // pairing to fewer seats than it was played with.
                    PlayerSummary {
                        player_key: key.clone(),
                        display_name: key.clone(),
                        dropped: false,
                    }
                })
        };
        Self {
            summary: TournamentSummary::from(meta),
            players: meta.players.iter().map(PlayerSummary::from).collect(),
            pairings: meta
                .pairings
                .iter()
                .map(|pairing| PairingView {
                    id: pairing.id,
                    round: pairing.round,
                    players: pairing.players.iter().map(player_summary).collect(),
                    outcome: pairing.outcome.clone(),
                })
                .collect(),
            standings: meta.standings(),
        }
    }
}

/// The lobby subset of `server_core::protocol::ClientMessage`. Wire-compatible:
/// the `type`/`data` tags and field shapes match the canonical enum exactly.
///
/// Deserialize incoming frames with [`parse_lobby_client_message`], NOT a bare
/// `serde_json::from_str` — that routes unknown tags to the reject path.
/// (No `PartialEq`: `DeckData` is not `PartialEq`, matching the canonical
/// `ClientMessage`, which is also not `PartialEq`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum LobbyClientMessage {
    ClientHello {
        client_version: String,
        build_commit: String,
        protocol_version: u32,
        /// The client's [`LOBBY_PROTOCOL_VERSION`]. `None` from clients built
        /// before the lobby owned its own version; those fall back to the
        /// `protocol_version` window. Additive and optional, so an older broker
        /// ignores it and an older client omits it — no `PROTOCOL_VERSION` bump
        /// is required for either direction to keep parsing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lobby_protocol_version: Option<u32>,
    },
    SubscribeLobby,
    UnsubscribeLobby,
    CreateGameWithSettings {
        deck: DeckData,
        display_name: String,
        public: bool,
        password: Option<String>,
        timer_seconds: Option<u32>,
        #[serde(default = "default_player_count")]
        player_count: u8,
        #[serde(default)]
        match_config: MatchConfig,
        #[serde(default)]
        format_config: Option<FormatConfig>,
        #[serde(default)]
        room_name: Option<String>,
        #[serde(default)]
        host_peer_id: Option<String>,
        #[serde(default)]
        draft_metadata: Option<DraftLobbyMetadata>,
        #[serde(default = "default_true")]
        start_when_full: bool,
        #[serde(default)]
        ranked: bool,
    },
    JoinGameWithPassword {
        game_code: String,
        deck: DeckData,
        display_name: String,
        password: Option<String>,
        #[serde(default)]
        reservation_token: Option<String>,
    },
    LookupJoinTarget {
        game_code: String,
        password: Option<String>,
        #[serde(default)]
        reserve: bool,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        release_reservation_token: Option<String>,
    },
    Ping {
        timestamp: u64,
    },
    UpdateLobbyMetadata {
        game_code: String,
        current_players: u8,
        max_players: u8,
        #[serde(default)]
        consumed_reservation_tokens: Vec<String>,
    },
    UnregisterLobby {
        game_code: String,
    },

    // --- Tournament organizer (lobby protocol 4) --------------------------
    //
    // Authority on every gated variant below is the TOKEN carried in the
    // payload, compared against the stored `organizer_token`/`player_token` —
    // never the socket's `ConnState`. That is the whole point of the model:
    // closing and reopening a connection must not cost an organizer their
    // event or a player their standing.
    /// Create a tournament. The broker mints the code and the
    /// `organizer_token`; the client chooses only the shape.
    CreateTournament {
        name: String,
        arity: crate::tournament::MatchArity,
        scoring: crate::tournament::ScoringPolicy,
        bracket: crate::tournament::BracketShape,
        /// Organizer override for the scheduled round count. `None` uses the
        /// bracket- and arity-selected default.
        #[serde(default)]
        total_rounds: Option<u32>,
    },
    /// Register as an entrant. `player_key` is **client-supplied** and opaque
    /// to the broker — the stable per-entrant identity, following
    /// `host_peer_id`'s precedent. The `player_token` that authorizes this
    /// entrant's later actions is minted by the broker in reply.
    JoinTournament {
        code: String,
        player_key: String,
        display_name: String,
    },
    /// Read one tournament's current view. Ungated: a tournament is public
    /// once its code is known, exactly like a lobby listing.
    GetTournament {
        code: String,
    },
    /// Organizer-gated: pair the next round.
    StartTournamentRound {
        code: String,
        organizer_token: String,
    },
    /// Player-gated: report a played pairing's result. The token must belong
    /// to a player seated in THIS pairing, not merely to some entrant.
    ReportMatchResult {
        code: String,
        pairing_id: crate::tournament::PairingId,
        player_token: String,
        outcome: crate::tournament::PodOutcome,
    },
    /// Player-gated: drop the token's owner from the event.
    DropFromTournament {
        code: String,
        player_token: String,
    },
    /// Organizer-gated: freeze the event as `Completed`.
    EndTournament {
        code: String,
        organizer_token: String,
    },
}

fn default_player_count() -> u8 {
    2
}

fn default_true() -> bool {
    true
}

/// The lobby subset of `server_core::protocol::ServerMessage`. Includes the
/// point-reply variants (`ServerHello`, `GameCreated`, `PeerInfo`,
/// `JoinTargetInfo`, `Error`, `Pong`, `PasswordRequired`) AND the fan-out
/// variants (`LobbyUpdate`, `LobbyGame{Added,Updated,Removed}`, `PlayerCount`).
/// Wire-compatible with the canonical enum.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum LobbyServerMessage {
    ServerHello {
        server_version: String,
        build_commit: String,
        protocol_version: u32,
        mode: ServerMode,
        /// This broker's [`LOBBY_PROTOCOL_VERSION`]. Advertised alongside — not
        /// instead of — `protocol_version`, which older clients still gate on
        /// and which therefore must keep tracking the full-game constant.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lobby_protocol_version: Option<u32>,
    },
    GameCreated {
        game_code: String,
        player_token: String,
    },
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<ServerErrorCode>,
    },
    LobbyUpdate {
        games: Vec<LobbyGame>,
    },
    LobbyGameAdded {
        game: LobbyGame,
    },
    LobbyGameUpdated {
        game: LobbyGame,
    },
    LobbyGameRemoved {
        game_code: String,
    },
    PlayerCount {
        count: u32,
    },
    PasswordRequired {
        game_code: String,
    },
    JoinTargetInfo {
        game_code: String,
        is_p2p: bool,
        #[serde(default)]
        format_config: Option<FormatConfig>,
        #[serde(default)]
        match_config: MatchConfig,
        player_count: u8,
        filled_seats: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_expires_at_ms: Option<u64>,
    },
    Pong {
        timestamp: u64,
    },
    PeerInfo {
        game_code: String,
        host_peer_id: String,
        #[serde(default)]
        format_config: Option<FormatConfig>,
        #[serde(default)]
        match_config: MatchConfig,
        player_count: u8,
        filled_seats: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_token: Option<String>,
    },

    // --- Tournament organizer (lobby protocol 4) --------------------------
    //
    // `TournamentCreated`/`TournamentJoined` are the ONLY two variants that
    // carry a token, and both are point replies to the caller who just earned
    // it. Every broadcast variant below carries a [`TournamentView`] or
    // [`TournamentSummary`], neither of which has a token field to leak.
    /// Point reply to `CreateTournament`. Carries the minted `organizer_token`
    /// — never broadcast.
    TournamentCreated {
        code: String,
        organizer_token: String,
        view: TournamentView,
    },
    /// Point reply to `JoinTournament`. Carries this entrant's minted
    /// `player_token` — never broadcast.
    TournamentJoined {
        code: String,
        player_token: String,
        view: TournamentView,
    },
    /// One tournament's detail view changed. Also the point reply to
    /// `GetTournament`.
    TournamentUpdate {
        code: String,
        view: TournamentView,
    },
    /// A tournament record is gone (a stale `Registration`, or a terminal
    /// event past its retention window).
    TournamentRemoved {
        code: String,
    },
    /// The full list of tournaments this broker holds. Emitted once per
    /// list-affecting change, and once on `SubscribeLobby`.
    TournamentListUpdate {
        tournaments: Vec<TournamentSummary>,
    },
}

impl LobbyServerMessage {
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            code: None,
        }
    }
}

/// Advertised role of the server. Mirrors `server_core::protocol::ServerMode`
/// exactly (same variants, same serde shape) so `ServerHello` is wire-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMode {
    Full,
    LobbyOnly,
}

/// Two-stage parse envelope: pull the `type` tag and keep `data` as raw JSON,
/// so an unrecognized tag can be rejected explicitly rather than failing the
/// whole parse (or, worse, collapsing into a magic variant via the
/// `#[serde(other)]` mechanism that is invalid on adjacently-tagged enums).
#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(rename = "type")]
    tag: String,
    #[serde(borrow)]
    data: Option<&'a serde_json::value::RawValue>,
}

/// Outcome of parsing an incoming lobby frame.
#[derive(Debug)]
pub enum ParsedFrame {
    /// A recognized lobby message. Boxed because `LobbyClientMessage` is far
    /// larger than the string variants (clippy `large_enum_variant`).
    Message(Box<LobbyClientMessage>),
    /// The frame was malformed JSON, a recognized tag whose `data` failed to
    /// deserialize, or a well-formed frame whose field values exceeded the
    /// bounds in [`crate::validation`]. Carries a human-readable reason for the
    /// `Error` reply.
    Malformed(String),
    /// The frame's `type` is not a known lobby tag. The shell routes this to
    /// the same reject path as a mode-disabled message.
    UnknownTag(String),
}

/// The set of `type` tags this broker recognizes. Kept as a function (not a
/// const slice match) so it stays trivially in sync with the enum variants —
/// every arm of [`deserialize_variant`] has a matching entry here.
///
/// **This gate is NOT compile-checked.** Unlike the exhaustive `match`es over
/// [`LobbyClientMessage`] elsewhere in this crate, a variant added to the enum
/// without an entry here still compiles — and then every frame carrying it is
/// answered [`ParsedFrame::UnknownTag`], as if a client had invented the tag.
/// `every_client_variant_tag_is_known` in this module's tests is the standing
/// guard: it asserts a representative frame for each variant round-trips
/// rather than enumerating the strings a second time.
fn is_known_lobby_tag(tag: &str) -> bool {
    matches!(
        tag,
        "ClientHello"
            | "SubscribeLobby"
            | "UnsubscribeLobby"
            | "CreateGameWithSettings"
            | "JoinGameWithPassword"
            | "LookupJoinTarget"
            | "Ping"
            | "UpdateLobbyMetadata"
            | "UnregisterLobby"
            | "CreateTournament"
            | "JoinTournament"
            | "GetTournament"
            | "StartTournamentRound"
            | "ReportMatchResult"
            | "DropFromTournament"
            | "EndTournament"
    )
}

/// Parse an incoming WebSocket text frame into a [`ParsedFrame`]. Unknown tags
/// route to [`ParsedFrame::UnknownTag`] (reject), malformed JSON or bad payload
/// to [`ParsedFrame::Malformed`].
pub fn parse_lobby_client_message(text: &str) -> ParsedFrame {
    let envelope: Envelope = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(e) => return ParsedFrame::Malformed(e.to_string()),
    };

    if !is_known_lobby_tag(&envelope.tag) {
        return ParsedFrame::UnknownTag(envelope.tag);
    }

    // Re-serialize the {type, data} pair and let the adjacently-tagged enum's
    // own deserializer handle it. This keeps a single source of truth for the
    // field-level deserialization (defaults, renames) rather than duplicating
    // every variant's field parsing here.
    let data_json = envelope.data.map(|d| d.get()).unwrap_or("null");
    let reconstructed = format!(
        r#"{{"type":{},"data":{}}}"#,
        json_string(&envelope.tag),
        data_json
    );
    match serde_json::from_str::<LobbyClientMessage>(&reconstructed) {
        Ok(msg) => match crate::validation::validate_lobby_message(&msg) {
            Ok(()) => ParsedFrame::Message(Box::new(msg)),
            Err(reason) => ParsedFrame::Malformed(reason),
        },
        Err(e) => ParsedFrame::Malformed(e.to_string()),
    }
}

/// Serialize a string as a JSON string literal (quotes + escaping).
fn json_string(s: &str) -> String {
    serde_json::to_string(s).expect("string always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two surfaces must not share a number. `scripts/check-protocol-version.mjs`
    /// carries the structural half of this guard — it matches
    /// `LOBBY_PROTOCOL_VERSION` only against a bare integer literal, so
    /// re-deriving it from `PROTOCOL_VERSION` fails the cross-language gate
    /// rather than silently re-coupling the lobby to full-game churn.
    #[test]
    fn lobby_protocol_version_is_independent_of_the_full_game_one() {
        assert_eq!(LOBBY_PROTOCOL_VERSION, 4);
        // Deliberately still 2, not 4: lobby versions 3 and 4 are purely
        // additive, so a version-2 client parses every frame it already
        // understood and is not evicted. See the constant's own changelog.
        assert_eq!(MIN_SUPPORTED_LOBBY_PROTOCOL, 2);
        assert_ne!(
            LOBBY_PROTOCOL_VERSION, PROTOCOL_VERSION,
            "the lobby must version its own message set, not alias the full-game one"
        );
        // A `const` block, not a runtime assert: both operands are constants, so
        // this is decidable at compile time. A floor above the current version
        // would refuse every client, and that should fail the BUILD rather than
        // wait for someone to run the test suite.
        const {
            assert!(
                MIN_SUPPORTED_LOBBY_PROTOCOL <= LOBBY_PROTOCOL_VERSION,
                "a floor above the current version would refuse every client"
            )
        };
    }

    #[test]
    fn protocol_version_tracks_full_game_wire_additions() {
        assert_eq!(PROTOCOL_VERSION, 63);
        // Lobby keeps its one-version rollout window; full-game servers stay
        // current-only (`server_core::MIN_SUPPORTED_PROTOCOL == PROTOCOL_VERSION`),
        // which is what refuses an older full-game peer whose GameState cannot
        // understand a success acknowledgment the submitting client awaits.
        assert_eq!(MIN_SUPPORTED_PROTOCOL, 62);
    }

    #[test]
    fn known_tags_parse_to_messages() {
        let frame = r#"{"type":"Ping","data":{"timestamp":42}}"#;
        match parse_lobby_client_message(frame) {
            ParsedFrame::Message(msg) => match *msg {
                LobbyClientMessage::Ping { timestamp } => assert_eq!(timestamp, 42),
                other => panic!("expected Ping, got {other:?}"),
            },
            other => panic!("expected Ping, got {other:?}"),
        }
    }

    #[test]
    fn unit_variant_with_no_data_parses() {
        let frame = r#"{"type":"SubscribeLobby"}"#;
        match parse_lobby_client_message(frame) {
            ParsedFrame::Message(msg) => {
                assert!(matches!(*msg, LobbyClientMessage::SubscribeLobby))
            }
            other => panic!("expected SubscribeLobby, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tag_routes_to_reject() {
        // `Action` is a real canonical tag but NOT a lobby tag — must reject,
        // not parse into a magic variant.
        let frame = r#"{"type":"Action","data":{"action":"PassPriority"}}"#;
        match parse_lobby_client_message(frame) {
            ParsedFrame::UnknownTag(tag) => assert_eq!(tag, "Action"),
            other => panic!("expected UnknownTag, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_routes_to_malformed() {
        assert!(matches!(
            parse_lobby_client_message("not json"),
            ParsedFrame::Malformed(_)
        ));
    }

    #[test]
    fn known_tag_with_bad_payload_routes_to_malformed() {
        let frame = r#"{"type":"Ping","data":{"timestamp":"not a number"}}"#;
        assert!(matches!(
            parse_lobby_client_message(frame),
            ParsedFrame::Malformed(_)
        ));
    }

    // --- Tournament wire surface (lobby protocol 4) -----------------------

    use crate::tournament::{
        BracketShape, MatchArity, PairingOutcome, PodOutcome, ScoringPolicy, TournamentMeta,
        TournamentPairing, TournamentPlayer, TournamentStatus,
    };

    /// The two secrets this whole surface must never broadcast. Used as
    /// needles in the leak assertions below, so a leak is caught by the token
    /// VALUE appearing in the bytes, not by a field name a rename could dodge.
    const ORGANIZER_SECRET: &str = "organizer-secret-do-not-leak";
    const PLAYER_A_SECRET: &str = "player-a-secret-do-not-leak";
    const PLAYER_B_SECRET: &str = "player-b-secret-do-not-leak";

    /// A tournament with real tokens, two entrants (one dropped), and a
    /// resolved round-1 pairing — enough shape that a projection which merely
    /// forgot to populate a field cannot pass the leak tests vacuously.
    fn meta_fixture() -> TournamentMeta {
        TournamentMeta {
            code: "TOUR01".to_string(),
            name: "Friday Night".to_string(),
            organizer_token: ORGANIZER_SECRET.to_string(),
            arity: MatchArity::HEAD_TO_HEAD,
            scoring: ScoringPolicy::default(),
            bracket: BracketShape::Swiss,
            total_rounds_override: Some(3),
            resolved_total_rounds: None,
            current_round: 1,
            status: TournamentStatus::InProgress,
            players: vec![
                TournamentPlayer {
                    player_key: "key-a".to_string(),
                    player_token: PLAYER_A_SECRET.to_string(),
                    display_name: "Alice".to_string(),
                    dropped: false,
                },
                TournamentPlayer {
                    player_key: "key-b".to_string(),
                    player_token: PLAYER_B_SECRET.to_string(),
                    display_name: "Bob".to_string(),
                    dropped: true,
                },
            ],
            pairings: vec![TournamentPairing {
                id: 0,
                round: 1,
                players: vec!["key-a".to_string(), "key-b".to_string()],
                outcome: Some(PairingOutcome::Reported(PodOutcome::Decisive {
                    winner: "key-a".to_string(),
                    game_wins: [("key-a".to_string(), 2u8), ("key-b".to_string(), 1u8)]
                        .into_iter()
                        .collect(),
                })),
            }],
            created_at: 1_000,
            last_activity_at: 2_000,
        }
    }

    /// The tournament additions follow the existing `FormatConfig` capability
    /// bump, so they consume the next independent lobby wire version.
    #[test]
    fn tournament_lobby_version_follows_the_format_config_bump() {
        const PRE_TOURNAMENT_LOBBY_VERSION: u32 = 3;
        assert_eq!(LOBBY_PROTOCOL_VERSION, PRE_TOURNAMENT_LOBBY_VERSION + 1);
    }

    /// The guard for [`is_known_lobby_tag`], which is a string `matches!` and
    /// therefore NOT compile-checked against the enum. A variant added without
    /// a tag entry parses to `UnknownTag` and is silently never dispatched;
    /// this test is the only thing that catches that.
    #[test]
    fn every_client_variant_tag_is_known() {
        // One representative frame per tournament variant, as a client would
        // actually send it.
        let frames = [
            (
                "CreateTournament",
                r#"{"type":"CreateTournament","data":{"name":"Friday Night","arity":2,"scoring":{"win_points":3,"draw_points":1,"loss_points":0},"bracket":"Swiss","total_rounds":3}}"#,
            ),
            (
                "JoinTournament",
                r#"{"type":"JoinTournament","data":{"code":"TOUR01","player_key":"key-a","display_name":"Alice"}}"#,
            ),
            (
                "GetTournament",
                r#"{"type":"GetTournament","data":{"code":"TOUR01"}}"#,
            ),
            (
                "StartTournamentRound",
                r#"{"type":"StartTournamentRound","data":{"code":"TOUR01","organizer_token":"tok"}}"#,
            ),
            (
                "ReportMatchResult",
                r#"{"type":"ReportMatchResult","data":{"code":"TOUR01","pairing_id":0,"player_token":"tok","outcome":{"Decisive":{"winner":"key-a","game_wins":{"key-a":2,"key-b":1}}}}}"#,
            ),
            (
                "DropFromTournament",
                r#"{"type":"DropFromTournament","data":{"code":"TOUR01","player_token":"tok"}}"#,
            ),
            (
                "EndTournament",
                r#"{"type":"EndTournament","data":{"code":"TOUR01","organizer_token":"tok"}}"#,
            ),
        ];

        for (tag, frame) in frames {
            match parse_lobby_client_message(frame) {
                ParsedFrame::Message(_) => {}
                other => panic!(
                    "{tag} must parse to a Message — an UnknownTag here means \
                     is_known_lobby_tag was not extended: {other:?}"
                ),
            }
        }
    }

    /// Discriminates the test above from a vacuous pass: an invented tag with
    /// the same shape must still be refused, so `every_client_variant_tag_is_known`
    /// cannot be satisfied by removing the gate entirely.
    #[test]
    fn an_invented_tournament_tag_is_still_unknown() {
        let frame = r#"{"type":"CancelTournament","data":{"code":"TOUR01"}}"#;
        match parse_lobby_client_message(frame) {
            ParsedFrame::UnknownTag(tag) => assert_eq!(tag, "CancelTournament"),
            other => panic!("expected UnknownTag, got {other:?}"),
        }
    }

    #[test]
    fn tournament_client_variants_round_trip_through_serde() {
        let messages = vec![
            LobbyClientMessage::CreateTournament {
                name: "Friday Night".to_string(),
                arity: MatchArity::COMMANDER_POD,
                scoring: ScoringPolicy::default_for_arity(MatchArity::COMMANDER_POD),
                bracket: BracketShape::Swiss,
                total_rounds: Some(4),
            },
            LobbyClientMessage::JoinTournament {
                code: "TOUR01".to_string(),
                player_key: "key-a".to_string(),
                display_name: "Alice".to_string(),
            },
            LobbyClientMessage::GetTournament {
                code: "TOUR01".to_string(),
            },
            LobbyClientMessage::StartTournamentRound {
                code: "TOUR01".to_string(),
                organizer_token: "tok".to_string(),
            },
            LobbyClientMessage::ReportMatchResult {
                code: "TOUR01".to_string(),
                pairing_id: 7,
                player_token: "tok".to_string(),
                outcome: PodOutcome::Draw,
            },
            LobbyClientMessage::DropFromTournament {
                code: "TOUR01".to_string(),
                player_token: "tok".to_string(),
            },
            LobbyClientMessage::EndTournament {
                code: "TOUR01".to_string(),
                organizer_token: "tok".to_string(),
            },
        ];

        for msg in messages {
            let json = serde_json::to_string(&msg).expect("serializes");
            match parse_lobby_client_message(&json) {
                ParsedFrame::Message(parsed) => {
                    // Compare through the serialized form: `LobbyClientMessage`
                    // is deliberately not `PartialEq` (`DeckData` is not).
                    assert_eq!(
                        serde_json::to_string(&*parsed).expect("re-serializes"),
                        json
                    );
                }
                other => panic!("{json} did not round-trip: {other:?}"),
            }
        }
    }

    #[test]
    fn tournament_server_variants_round_trip_through_serde() {
        let view = TournamentView::from(&meta_fixture());
        let messages = vec![
            LobbyServerMessage::TournamentCreated {
                code: "TOUR01".to_string(),
                organizer_token: ORGANIZER_SECRET.to_string(),
                view: view.clone(),
            },
            LobbyServerMessage::TournamentJoined {
                code: "TOUR01".to_string(),
                player_token: PLAYER_A_SECRET.to_string(),
                view: view.clone(),
            },
            LobbyServerMessage::TournamentUpdate {
                code: "TOUR01".to_string(),
                view: view.clone(),
            },
            LobbyServerMessage::TournamentRemoved {
                code: "TOUR01".to_string(),
            },
            LobbyServerMessage::TournamentListUpdate {
                tournaments: vec![view.summary.clone()],
            },
        ];

        for msg in messages {
            let json = serde_json::to_string(&msg).expect("serializes");
            let back: LobbyServerMessage = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(back, msg);
        }
    }

    /// The whole reason the view types exist. A structural assertion on the
    /// serialized BYTES: neither token value may appear anywhere in a view,
    /// however deeply nested.
    #[test]
    fn tournament_views_never_carry_a_token() {
        let meta = meta_fixture();
        let view = TournamentView::from(&meta);
        let json = serde_json::to_string(&view).expect("serializes");

        for secret in [ORGANIZER_SECRET, PLAYER_A_SECRET, PLAYER_B_SECRET] {
            assert!(
                !json.contains(secret),
                "TournamentView leaked {secret}: {json}"
            );
        }
        // Non-vacuity: the view really did carry this tournament's content,
        // so the assertion above ran against a populated projection.
        assert!(json.contains("Alice") && json.contains("Bob"));
        assert!(json.contains("Friday Night"));
        assert_eq!(view.players.len(), 2);
        assert_eq!(view.pairings.len(), 1);
        assert_eq!(view.pairings[0].players.len(), 2);
        assert_eq!(view.standings.len(), 2);
    }

    /// The broadcast path specifically: a `TournamentListUpdate` and a
    /// `TournamentUpdate` are fanned out to every subscriber, so neither may
    /// carry a secret even though `TournamentCreated`/`TournamentJoined`
    /// (point replies) legitimately do.
    #[test]
    fn broadcast_tournament_messages_never_carry_a_token() {
        let meta = meta_fixture();
        let view = TournamentView::from(&meta);
        let broadcasts = [
            LobbyServerMessage::TournamentUpdate {
                code: meta.code.clone(),
                view: view.clone(),
            },
            LobbyServerMessage::TournamentListUpdate {
                tournaments: vec![TournamentSummary::from(&meta)],
            },
            LobbyServerMessage::TournamentRemoved {
                code: meta.code.clone(),
            },
        ];

        for msg in broadcasts {
            let json = serde_json::to_string(&msg).expect("serializes");
            for secret in [ORGANIZER_SECRET, PLAYER_A_SECRET, PLAYER_B_SECRET] {
                assert!(!json.contains(secret), "{json} leaked {secret}");
            }
        }
    }

    /// `player_count` on a list row counts ACTIVE entrants, and the detail
    /// view still carries the dropped one. Pins the documented choice so a
    /// later edit cannot silently swap it for `players.len()`.
    #[test]
    fn summary_counts_active_players_while_the_view_keeps_dropped_ones() {
        let meta = meta_fixture();
        let summary = TournamentSummary::from(&meta);
        assert_eq!(summary.player_count, 1, "Bob dropped, so one active");
        assert_eq!(meta.players.len(), 2);

        let view = TournamentView::from(&meta);
        assert_eq!(view.players.len(), 2, "the detail view keeps history");
        assert!(view.players.iter().any(|p| p.dropped));
    }

    /// Hostile fixture for Verification Matrix row 9's `PairingView` case:
    /// every `PairingOutcome` shape must survive projection and serde without
    /// collapsing to a default.
    #[test]
    fn every_pairing_outcome_survives_the_view_projection() {
        let mut meta = meta_fixture();
        meta.pairings = vec![
            TournamentPairing {
                id: 0,
                round: 1,
                players: vec!["key-a".to_string()],
                outcome: Some(PairingOutcome::Bye),
            },
            TournamentPairing {
                id: 1,
                round: 1,
                players: vec!["key-a".to_string(), "key-b".to_string()],
                outcome: Some(PairingOutcome::Forfeit {
                    winner: "key-a".to_string(),
                }),
            },
            TournamentPairing {
                id: 2,
                round: 1,
                players: vec!["key-a".to_string(), "key-b".to_string()],
                outcome: Some(PairingOutcome::Reported(PodOutcome::Decisive {
                    winner: "key-b".to_string(),
                    game_wins: [("key-a".to_string(), 1u8), ("key-b".to_string(), 2u8)]
                        .into_iter()
                        .collect(),
                })),
            },
            TournamentPairing {
                id: 3,
                round: 1,
                players: vec!["key-a".to_string(), "key-b".to_string()],
                outcome: Some(PairingOutcome::Reported(PodOutcome::Draw)),
            },
            TournamentPairing {
                id: 4,
                round: 2,
                players: vec!["key-a".to_string(), "key-b".to_string()],
                outcome: None,
            },
        ];

        let view = TournamentView::from(&meta);
        let json = serde_json::to_string(&view).expect("serializes");
        let back: TournamentView = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, view);

        let outcomes: Vec<_> = view.pairings.iter().map(|p| p.outcome.clone()).collect();
        assert_eq!(outcomes[0], Some(PairingOutcome::Bye));
        assert!(matches!(outcomes[1], Some(PairingOutcome::Forfeit { .. })));
        assert!(matches!(
            outcomes[2],
            Some(PairingOutcome::Reported(PodOutcome::Decisive { .. }))
        ));
        assert_eq!(
            outcomes[3],
            Some(PairingOutcome::Reported(PodOutcome::Draw))
        );
        assert_eq!(outcomes[4], None, "a pending pairing stays pending");
    }

    #[test]
    fn well_formed_frame_with_out_of_bounds_field_routes_to_malformed() {
        // Valid JSON and a known tag, but the display name exceeds the bound,
        // so validation rejects it at the parse boundary.
        let long_name = "a".repeat(21);
        let frame = format!(
            r#"{{"type":"CreateGameWithSettings","data":{{"deck":{{"main_deck":[]}},"display_name":"{long_name}","public":true,"password":null,"timer_seconds":null}}}}"#
        );
        assert!(matches!(
            parse_lobby_client_message(&frame),
            ParsedFrame::Malformed(_)
        ));
    }
}
