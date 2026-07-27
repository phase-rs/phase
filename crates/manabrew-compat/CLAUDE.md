# manabrew-compat — Crate-Specific Guide

This guide is the **first thing any agent working on `crates/manabrew-compat/` should read.** It complements the workspace `CLAUDE.md` with rules and known-state specific to this crate. If something here conflicts with workspace `CLAUDE.md`, workspace wins — but the items below have been learned the hard way and should not be rediscovered.

## What this crate does

Translates between phase's engine types and the **ManaBrew wire protocol** (external MTG engine↔client JSON protocol, currently 2.0.0). It is a bidirectional adapter:

- **Outbound:** `WaitingFor` (engine decision point) → `PromptInput` (protocol prompt), plus `GameAction` → `AvailableActionKind`.
- **Inbound:** `PromptOutput` (client answer) → `GameAction`.

It is a **thin serialization boundary**. Per workspace `CLAUDE.md`, zero game logic lives here — no legality checks, no derived state, no rules inference. If the adapter needs a value, the engine must provide it.

The normative protocol source is the upstream `manabrew-protocol` crate. Verify every field against that crate's source, never against a summary, a memory, or this file.

## Hard rules — non-negotiable

### 1. Classify by payload type. Never by concept name.

**This is the rule that matters most, and it has been violated repeatedly.**

MTG mechanic names imply exotic structure. The decision underneath is almost always one of five ordinary shapes. Before concluding anything about a decision, read the **fields of the answering `GameAction`** and the doc comment describing what the engine derives.

Worked examples where the name misled and the payload did not:

| Decision | What the name suggests | What the payload is |
|---|---|---|
| `SeparatePilesPartition` (Fact or Fiction) | needs a "partition" primitive | `SubmitPilePartition { pile_a: Vec<ObjectId> }` — engine derives pile B as `eligible \ pile_a`, so it is **pick a subset** |
| `SeparatePilesChoice` | pile semantics | `ChoosePile { pile: PileSide }`, `PileSide = A \| B` — **pick one of two** |
| `MiracleReveal` | an alternative cost | a **yes/no** offer; the cast itself rides `Cast.label` |
| Ninjutsu | an alternative cost | CR 702.49a: an **activated ability** → `ActivateAbility` |

Corollary: a mechanic's absence from one protocol enum says nothing about support. Ninjutsu is absent from `AlternativeCostKind` and fully supported via `ActivateAbility`. **Name the enum you checked, then check the others.**

**The field list is not the whole payload — read the projection.** `UntapChoice` carries `candidates: Vec<ObjectId>`, which reads as a subset pick. It is not: `interaction.rs:2035-2057` projects it as `candidates.len() × 2` separate `ChooseUntap { object_id, untap }` actions — a per-permanent boolean answered one at a time. A field type narrows the possibilities; only the projection and the answering action settle it.

### 2. Never write "unsupported" without naming the population you searched.

Every `UnsupportedCapability` entry and every `Unsupported` call site is a **claim about the engine or about upstream**, and it will be read as an inventory of deficiency. Before writing one:

1. Grep the engine for the mechanic — parser module, effect module, `database/synthesis.rs`, `types/actions.rs`.
2. Check `git log --grep` for it. Several "missing" mechanics had shipped in named PRs.
3. Grep-verify any CR number you cite against `docs/MagicCompRules.txt` (workspace rule).
4. Only then write the entry — and phrase it as *"absent from `<enum>` (N variants); reachable instead via X"*, not as *"phase does not support X."*

`suggested_protocol_extension` is a request you are making of upstream maintainers. Do not ask for a shape that would encode a rules error (asking for `AlternativeCostKind::Ninjutsu` would have; CR 702.49a makes it an activated ability).

### 3. The capability registry must declare what the code actually emits.

`UNSUPPORTED_PROTOCOL_CAPABILITIES` is a public contract — consumers call `unsupported_protocol_capabilities()` to plan around gaps. Every capability code emitted anywhere in the crate must appear in it, or the consumer is blindsided at runtime.

Recompute the delta rather than trusting any number written down:

```bash
rg -o '"(local|upstream)\.[a-z0-9-]+"' crates/manabrew-compat/src/lib.rs | sort -u
```

Compare emitted codes against declared entries. Divergence is a defect, not a backlog item.

The registry is now **exhaustive** over emitted codes (82 declared; 69 emitted at live call sites, plus 13 documentary entries that describe a gap without a code path). `no_emitted_capability_code_is_undeclared` scans the production half of `lib.rs` and fails on any new undeclared code, so this no longer needs a manual audit — but re-run the command above if you doubt the test.

### 4. A mapping claim must be exhibited by a test, not asserted in prose.

"No exact upstream shape exists for this" is unfalsifiable-sounding and survives any number of review rounds, because reviewers verify that a design is internally coherent — they do not re-derive its premise. A test that **constructs** the prompt and translates the response back dies immediately if the claim is wrong.

When you map a new family, add a case to the mapping test that asserts prompt construction *and* response translation, including both branches of a boolean. Watch it fail before it passes.

Serialized prompts nest under `input`: assert `json["input"]["type"]`, not `json["type"]`.

### 5. Response translation dispatches on `state.waiting_for`.

Generic prompt families are many-to-one: one `ChooseBoolean` answers several different engine questions. `translate_response` must therefore match on the current `WaitingFor` to pick the right `GameAction`, and the gate (`PromptOutput` → permitted `WaitingFor` set) must list every variant that family can answer. Adding a mapping means touching **three** places — prompt construction, response translation, and the gate. Missing the gate makes a legal answer illegal; missing translation makes the prompt unanswerable.

## Before hand-mapping anything: the engine's interaction subsystem

**The engine already owns a public, generic, viewer-safe decision API. Use it before writing
a per-`WaitingFor` mapping.**

- `engine::game::interaction::derive_viewer_interaction(authoritative_state, filtered_state, viewer)
  -> ViewerInteraction` (`interaction.rs:6988`). Its doc: *"Authorization and capability identity
  are read only from `authoritative_state`; every object, card, zone, and presentation surface is
  read only from `filtered_state`."* It is **viewer-safe by construction** and already handles
  authorization, including turn-control (Mindslaver) via `authorized_submitters`.
- `engine::game::interaction::submit_interaction(state, actor, submission)` (`interaction.rs:8857`)
  takes an `InteractionSubmission { interaction_id, response }`, does its own viewer filtering,
  **materializes the `GameAction` itself** (`materialize_response`, `:8610`), and applies it.

`ViewerInteraction.opportunities[]` carries a response **spec** (`InteractionResponseSpec` —
`Select`/`AssignAmounts`/`AssignDamage`/`Sequence`/`GroupedSequence`/`ManaGroups`/`Number`/`Text`/…),
presentation `surfaces`, and `progress`. `InteractionResponse` has the matching ~10 answer shapes.
Constraints (`min`/`max`/`exact_total`, group constraints) come from the spec — **the engine
computes them**; deriving them in the adapter is duplicated game logic and violates the
thin-boundary rule.

Ten spec shapes map onto the protocol's families far more cheaply than 127 `WaitingFor` variants
do, and the per-variant `translate_response` dispatch largely disappears because the engine
materializes the action. If you find yourself about to hand-write a 100-row variant table, stop:
trace this subsystem first and record why it does not fit before doing it the hard way.

## Reverse dictionary: decision shape → prompt family

Verified against the upstream `prompts/` module. Match on the shape of the engine payload:

| Engine payload shape | Prompt family | Key fields |
|---|---|---|
| `Vec<ObjectId>`, non-targeting card selection | `ChooseCards` | `cards: Vec<CardDto>`, `min`, `max` |
| Objects **or** players, as CR 115 targets | `ChooseBoardTargets` | `candidates: Vec<TargetRef>`, `intent: TargetingIntent`, `min_targets`, `max_targets` |
| `Vec<PlayerId>` | `ChooseBoardTargets` | `TargetKind::Player` |
| Abstract labelled options (which cost, which mode) | `ChooseFromSelection` | `options: Vec<{label, weight, can_repeat}>`, `min_total`, `max_total` |
| Numeric amount | `ChooseNumber` | `min`, `max` |
| Unit / yes-no | `ChooseBoolean` | `confirm_label`, `deny_label` |
| Ordering | `Reorder` | `items: Vec<ReorderItem>` |
| Look-then-distribute | `Scry` | `cards`, `zones: Vec<ScryDestination>` |

Two distinctions that are easy to get wrong:

- **`ChooseBoardTargets` vs `ChooseCards`.** `ChooseBoardTargets` carries `intent`/`hostile` and is for **targeting** (CR 115). Non-targeting selection ("choose a card in your graveyard") is `ChooseCards`.
- **`ChooseFromSelection` is the generic escape hatch.** Labelled options with min/max totals. Its existence is why "this needs a new prompt family" is almost always the wrong conclusion. Reach for it before proposing an upstream extension.

`Reorder` item ids must match what the answering action indexes. `GameAction::OrderTriggers { order: Vec<usize> }` takes **indices** — using source object ids collides when one permanent contributes two triggers.

## Census protocol — how to find what is actually unmapped

Do not audit case-by-case; that is how pessimistic claims get made. Bucket every unmapped decision by payload type in one pass:

```bash
python3 - <<'PY'
import re
src = open('crates/engine/src/types/game_state.rs').read()
i = src.index('pub enum WaitingFor'); j = src.index('{', i); d = 0
for k in range(j, len(src)):
    d += (src[k] == '{') - (src[k] == '}')
    if d == 0: end = k; break
body = src[j+1:end]
variants, depth, cur = {}, 0, None
for l in body.split('\n'):
    if depth == 0:
        m = re.match(r'^([A-Z][A-Za-z0-9]*)\s*(\{|\(|,|$)', l.strip())
        if m: cur = m.group(1); variants[cur] = []
    if cur: variants[cur].append(l.strip())
    depth += l.count('{') + l.count('(') - l.count('}') - l.count(')')
used = set(re.findall(r'WaitingFor::([A-Za-z0-9]+)',
                      open('crates/manabrew-compat/src/lib.rs').read()))
for k, v in sorted(variants.items()):
    if k not in used: print(k, '|', ' '.join(v)[:110])
PY
```

Then read the answering `GameAction` for each and place it in the reverse-dictionary table above. A shape census takes minutes and catches your own pessimism; per-case judgement does not.

Orientation snapshot (2026-07-26, will drift — recompute): 127 `WaitingFor` variants, 34 referenced by the adapter. Of the 93 unmapped, ~84% bucketed onto an existing prompt family on payload shape alone.

## Protocol facts that constrain design

Confirmed by reading the upstream crate; re-verify when bumping protocol versions.

- **The wire is strictly closed.** No `#[non_exhaustive]`, no `#[serde(other)]` fallback on any tagged enum, and `deny_unknown_fields` on the transport envelope. Version skew is a hard deserialize failure, not a graceful degrade — and even adding an *optional* field is breaking for older readers.
- **`PROTOCOL_VERSION` is a single integer** derived from the crate's major version. There is no per-family capability negotiation.
- **Prompts carry no machine-readable discriminator.** `PromptPresentation` is `{title, description, text, targets}` — all free text. When many engine decisions funnel into one generic family, only the human-readable title distinguishes them. Consumers that reason programmatically (AI agents) must therefore parse prose. If you propose one upstream change, propose an optional namespaced `kind` field here — parameterize, don't proliferate.

## Known adapter-side blockers — both now closed

These were **not** protocol gaps; they were signature limitations in this crate, and both are fixed:

- `convert_available_action`, `available_actions`, and `advertised_action_by_id` now take `&GameState`. Ninjutsu is advertised as `AvailableActionKind::ActivateAbility` (CR 702.49a), one action per (ninjutsu card, returned attacker) pair, with the returned attacker named in the description (CR 702.49c). `ability_index` is descriptive only — the echo round-trips by action id, because driving the marker slot through `GameAction::ActivateAbility` would stack the ability without paying mana.
- Phyrexian payment needed no state after all: the engine already enumerates the shard routes as legal `SubmitPhyrexianChoices` actions, so `convert_payment_action` prices each route at `2 × PayLife shards` (CR 107.4f) and the shard list adds nothing. A single pending shard advertises exactly one `PayLife { amount: 2 }`.

The same "no ability index at the action level" shape still blocks `local.board-action-unsupported` (equip, crew, station, saddle, transform, turn face up), which is now unblocked by the threaded `GameState` and is the highest-value remaining item — those are priority-window plays, so filtering them out is real functional loss.

## Running the tests

```bash
cargo test -p manabrew-compat --features engine/test-support
```

The crate's own `[dev-dependencies]` now enable `engine/test-support`, so a bare `cargo test -p manabrew-compat` also works; before that it failed with `E0603` because `engine::game::zones` is `pub` only under `cfg(any(test, feature = "test-support"))`. CI needs no dedicated step: `rust-lint` runs `cargo clippy --workspace --all-targets` and `rust-test` runs `cargo nextest run --workspace`, both of which select this crate.

Per workspace `CLAUDE.md`, prefer Tilt for `clippy`/`test-engine` signal rather than running cargo directly; `cargo fmt --all` is always run directly.

## How to make progress

1. Run the census. Work the largest shape bucket, not the most interesting-sounding mechanic.
2. For each variant: read the answering `GameAction`, pick the family from the reverse dictionary, and wire **all three** places (prompt, translation, gate).
3. Add the mapping test case. Watch it fail first.
4. Reconcile the capability registry against emitted codes.
5. Never write "unsupported" without having named the population you searched.
