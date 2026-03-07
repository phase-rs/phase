# Domain Pitfalls

**Domain:** MTG Rules Engine (Rust + Tauri + WASM dual-target)
**Researched:** 2026-03-07

---

## Critical Pitfalls

Mistakes that cause rewrites or major issues. Each of these has wrecked MTG engine projects.

---

### Pitfall 1: Layer System Dependency Cycles (Rule 613)

**What goes wrong:** The seven-layer continuous effect evaluation (Rule 613) seems straightforward until dependency exceptions appear. Effect A in Layer 4 (type-changing) can make Effect B in the same layer applicable or inapplicable. The spec says "if applying one effect would change whether another effect applies, there's a dependency." Naive implementations either ignore dependencies (wrong results) or detect them incorrectly (infinite loops during evaluation).

**Why it happens:** Developers implement layers as a simple ordered loop and forget that within a single layer, effects can depend on each other. The dependency exception overrides timestamp ordering within a layer. Additionally, Layer 7 (power/toughness) has five sublayers (7a-7e) where characteristic-defining abilities, set-to effects, and +N/+N effects interact in non-obvious ways — a Tarmogoyf (7a, CDA) that gets Humility'd (7b, sets P/T) then Giant Growth'd (7d, +3/+3) has a specific correct answer.

**Consequences:** Wrong P/T calculations, wrong type assignments, wrong ability grants. Cards that "almost work" but produce wrong results in 5% of board states — the hardest bugs to find.

**Prevention:**
- Implement dependency detection as a separate pass before applying effects within each layer. If effect A's applicability changes based on effect B, A depends on B; apply B first regardless of timestamp.
- Port Forge's layer system tests verbatim as your first test suite — these encode decades of edge case discovery.
- Use the MTG Comprehensive Rules 613.8 (dependency rules) as your spec, not blog posts or summaries.
- Build a "layer debugger" that traces which effects applied in which order for any given board state.

**Detection:** Test with Humility + Opalescence, Blood Moon + Urborg, any cards that change types/abilities in the same layer. If results differ from MTGO/Arena, dependency handling is wrong.

**Confidence:** HIGH — this is the single most-documented failure mode in MTG engine development. The original Forge blog noted "the rules engine itself is the most complicated part."

**Phase:** Phase addressing static abilities and layer system. Must be the most heavily tested phase.

---

### Pitfall 2: State-Based Actions Recursive Check Loop

**What goes wrong:** SBAs must be checked repeatedly until none apply, then triggers go on the stack, then SBAs check again. Developers either check once (missing cascading SBAs) or check without a termination guard (infinite loop when SBAs create conditions for more SBAs).

**Why it happens:** The spec says: "check SBAs → perform all simultaneously → if any were performed, check again → once stable, put triggered abilities on stack → check SBAs again." This is a fixpoint loop. If your SBA handler has a bug where performing an SBA re-triggers the same condition (e.g., moving a creature to graveyard triggers another creature's -1/-1 but the state update isn't atomic), you loop forever.

**Consequences:** Game hangs, stack overflow, or incorrect game state where triggers fire before SBAs stabilize.

**Prevention:**
- Implement SBAs as a single atomic batch: collect all applicable SBAs, apply them all, then re-check. Never interleave SBA checks with individual SBA execution.
- Add a loop counter with a reasonable cap (e.g., 1000 iterations) and treat exceeding it as a draw (matching MTG rules for mandatory action loops, Rule 104.4b).
- SBAs must not use the stack and must not give priority — they are invisible to the priority system.
- Test with scenarios that cascade: a board full of 1/1 tokens where a -X/-X effect kills some, creating death triggers that kill more.

**Detection:** Any game that hangs during resolution likely has an SBA fixpoint bug.

**Confidence:** HIGH — directly from MTG Comprehensive Rules and multiple rules engine post-mortems.

**Phase:** Core game state engine phase. Get this right before adding any card abilities.

---

### Pitfall 3: Replacement Effect Self-Application and "Apply Once" Rule

**What goes wrong:** Replacement effects modify events before they happen. A replacement effect can only apply once to any given event. Two doublers (e.g., two "if a creature would deal damage, it deals double instead") produce 4x damage, not infinite. Developers who don't track "already applied this replacement to this event" get infinite loops or wrong multipliers.

**Why it happens:** The implementation needs per-event tracking of which replacement effects have already modified that specific event instance. Without this, the system re-applies the same replacement after it modified the event, seeing "oh, damage is happening, I should double it" again.

**Consequences:** Infinite loops, wrong damage calculations, game crashes during replacement effect resolution.

**Prevention:**
- Every pending event gets a unique ID. Each replacement effect records which event IDs it has already modified. A replacement cannot modify an event it has already modified.
- When multiple replacement effects apply to the same event, the affected player (or controller of the affected object) chooses the order — this requires a UI prompt.
- Implement replacement effects as a pipeline: event enters, eligible replacements are collected, player chooses one, it modifies the event, re-check for remaining eligible replacements (excluding the one just applied), repeat until none remain.

**Detection:** Test with two damage doublers, two ETB replacement effects, prevention shields + damage redirection. If damage goes infinite or effects apply more than once, the tracking is broken.

**Confidence:** HIGH — Rule 614.5 explicitly states this. Confirmed in multiple MTG rules forums.

**Phase:** Replacement effects phase. Must be architecturally solid before trigger system, since triggers fire after events and replacements modify events before they happen.

---

### Pitfall 4: Rust Ownership vs. Game State Mutation During Effect Resolution

**What goes wrong:** MTG effect resolution frequently needs to read the game state while simultaneously mutating it. Example: "Destroy all creatures with power less than X, where X is the number of creatures you control." You need to read the battlefield to calculate X, then mutate the battlefield to destroy creatures. Rust's borrow checker prevents holding an immutable reference (for reading) and a mutable reference (for writing) simultaneously.

**Why it happens:** Game engines naturally want a "god object" game state that everything reads from and writes to. Rust's ownership model fundamentally rejects this pattern. Naive approaches either clone the entire state for every read (performance disaster at MTG scale) or use `RefCell`/interior mutability everywhere (loses compile-time safety, panics at runtime).

**Consequences:** Either the codebase is littered with `.clone()` calls destroying performance, or `RefCell<>` wrappers creating runtime panics, or unsafe blocks undermining Rust's safety guarantees.

**Prevention:**
- **Command buffer pattern:** Effects don't mutate state directly. They return a list of state mutations (commands) that are applied after all reads are complete. This is the ECS-standard pattern and maps perfectly to the reducer/action architecture already planned.
- **Snapshot reads:** Before resolving an effect, snapshot the relevant state (not the entire state — just what the effect needs to query). The effect reads from the snapshot and produces mutations applied to the live state.
- **Arena-based allocation:** Use an arena allocator for temporary game objects during resolution (e.g., `bumpalo`). Allocations are fast and freed in bulk.
- The planned immutable state + reducer architecture already mitigates this — lean into it. Effects are pure functions: `(state, effect) -> Vec<StateMutation>`.

**Detection:** If you're fighting the borrow checker on every effect handler, the architecture is wrong. Effect handlers should take `&GameState` (immutable) and return mutations, never `&mut GameState`.

**Confidence:** HIGH — well-documented Rust game development pattern. The project's planned reducer architecture is the correct mitigation.

**Phase:** Core game state engine design phase. Architecture decision that pervades everything.

---

### Pitfall 5: WASM Binary Size Explosion

**What goes wrong:** A Rust WASM binary for a full MTG engine can easily reach 10-20MB+ without optimization. At that size, PWA load times are unacceptable (especially on tablet over WiFi). One documented case showed a 3.2MB WASM module that was 9x larger than the equivalent C implementation.

**Why it happens:** Multiple compounding factors:
- Rust monomorphizes generics — every `Vec<T>` for a different T generates separate code
- The default allocator (dlmalloc port) adds ~10KB baseline
- `serde` derive macros generate substantial code for every type
- `std::fmt` machinery (used by `format!`, `panic!` messages) pulls in significant code
- Debug info and panic strings are included by default
- String formatting for error messages adds hidden bloat

**Consequences:** PWA is unusable on slower connections. Mobile Safari may refuse to compile very large WASM modules. Users abandon before the game loads.

**Prevention:**
- Configure release profile: `panic = 'abort'`, `opt-level = 'z'`, `lto = true`, `codegen-units = 1`, `strip = true`
- Use `wasm-opt` (from binaryen) as a post-processing step — typically saves 10-20% additional
- Profile with `twiggy` to identify what's taking space before optimizing blindly
- Consider `talc` allocator instead of dlmalloc (smaller and faster)
- Minimize serde usage across the WASM boundary — prefer opaque handles with getter/setter methods over serializing entire game state
- Avoid `format!` and `println!` in engine code — use error codes instead of formatted error strings
- Lazy-load card definitions (don't compile 32k cards into the WASM binary)
- Set a size budget early (target: <3MB for engine WASM, ideally <1.5MB) and track it in CI

**Detection:** Add WASM binary size to CI metrics from day one. If it exceeds budget, investigate immediately rather than deferring.

**Confidence:** HIGH — multiple documented sources with specific techniques.

**Phase:** Must be addressed from the very first WASM build. Retrofitting size optimization is far harder than building with it from the start.

---

### Pitfall 6: Tauri IPC Serialization Bottleneck

**What goes wrong:** Every communication between the Rust engine (backend) and React UI (frontend) goes through Tauri's IPC layer. In v1, this was JSON serialization. V2 improved this with custom protocols, but serializing full game state (battlefield with 20+ permanents, each with counters, attachments, modifications, continuous effects) on every state change creates noticeable lag.

**Why it happens:** MTG game state is 10-50x larger than simple card games. A single spell resolution can trigger cascading state changes (spell resolves → triggers fire → SBAs check → more triggers). If each intermediate state is serialized and sent to the UI, the IPC becomes the bottleneck.

**Consequences:** UI lag during complex resolution sequences. Animations stutter. The game feels sluggish despite the Rust engine being fast.

**Prevention:**
- **Delta updates, not full state:** Send only what changed, not the entire game state. Use a diffing mechanism or event-based updates ("card X moved from zone A to zone B", "player life changed to N").
- **Batch updates:** During resolution sequences, batch all state changes and send a single update when a priority window opens (when the player can actually act).
- **Use Tauri v2 raw byte protocol** for large payloads instead of JSON serialization.
- **Keep complex state queries in Rust:** Don't send the full layer-evaluated state to JS. Instead, expose queries: "what is card X's current P/T?" that the frontend calls as needed.
- **Consider SharedArrayBuffer** for the WASM PWA target — shared memory between WASM and JS avoids serialization entirely.

**Detection:** Profile IPC round-trip time. If a single game action takes >16ms for the UI to reflect (one frame at 60fps), the IPC is the bottleneck.

**Confidence:** MEDIUM-HIGH — Tauri v2's improvements help, but MTG's state complexity is unusual even among card games.

**Phase:** UI integration phase. Design the IPC protocol before building the UI, not after.

---

## Moderate Pitfalls

---

### Pitfall 7: Forge Card Format Multi-Face Card Parsing

**What goes wrong:** Forge's `.txt` card format handles multi-face cards (split cards, double-faced cards, adventure cards, MDFCs, meld cards, flip cards) with different conventions. Split cards use `//` in names. DFCs use separate files with `DeckHas:` references. Adventure cards have `ALTERNATE` sections. Each type has subtly different parsing rules.

**Prevention:**
- Catalog every multi-face card type Forge supports: Split, Flip, Transform (DFC), Meld, Adventure, MDFC, Aftermath, Fuse. Write a parser test for at least one card of each type.
- Parse the `AlternateMode:` field early — it determines how to interpret the rest of the card definition.
- Don't assume two faces = two files. Some multi-face types are single file with sections, others are separate files linked by reference.
- Handle encoding carefully — Forge card names include special characters (Lim-Dul's, Juzam Djinn accents in some localizations). Use UTF-8 throughout.

**Detection:** Run the parser against Forge's full `cardsfolder/` and count parse failures. Any card that fails to parse is likely a multi-face edge case.

**Confidence:** MEDIUM — based on Forge's known format conventions. Exact edge cases need validation against the actual card files.

**Phase:** Card parser phase. Build a comprehensive test matrix of card types before claiming the parser is complete.

---

### Pitfall 8: AI Game Tree Explosion in MTG

**What goes wrong:** MTG has a vastly higher branching factor than chess or even other card games. A single main phase can have 10+ legal plays (multiple cards in hand, multiple abilities on board, multiple mana payment options). With instant-speed interactions, each player can respond at every priority window. A 2-turn lookahead can produce millions of nodes.

**Prevention:**
- Do NOT attempt minimax/alpha-beta for MTG — the branching factor is too high. Use heuristic evaluation with limited lookahead (1 phase, not 1 turn).
- Start with a purely heuristic AI (board evaluation function, no search tree). This is how Forge's AI works for most decisions.
- Use Monte Carlo Tree Search (MCTS) only for critical decisions (combat, counterspell timing) where the branching factor is bounded.
- For WASM target, AI computation must not block the main thread. Use Web Workers (a separate WASM instance in a worker thread).
- Hidden information (opponent's hand, library order) makes perfect search impossible anyway. Use information set sampling (play out N random possible hands for the opponent).
- Set hard time limits (e.g., 2 seconds per decision) and return best-so-far when time expires.

**Detection:** If AI turns take more than 3 seconds on native or 5 seconds in WASM, the search is too deep.

**Confidence:** HIGH — fundamental game theory limitation. Forge's AI is ~57k LOC of heuristics, not tree search, for this exact reason.

**Phase:** AI foundation phase. Start with heuristics. Tree search is an optimization for specific decisions, not the core approach.

---

### Pitfall 9: WASM Threading and Main Thread Blocking

**What goes wrong:** Rust's `std::thread` does not work in `wasm32-unknown-unknown`. There is no concept of thread spawn, thread join, or thread-local storage in browser WASM. `Mutex::lock()` will panic on the main thread because the browser main thread cannot block. AI computation or complex resolution sequences that work fine in native Rust will hang or crash in the browser.

**Prevention:**
- Design the engine with an async interface from the start: `async fn resolve_next() -> StateUpdate` rather than blocking `fn resolve_all() -> FinalState`.
- Use Web Workers for AI computation — spawn a separate WASM instance in a worker thread and communicate via `postMessage`.
- Never use `std::sync::Mutex` in code that targets WASM. Use `std::cell::RefCell` for single-threaded contexts or design around message passing.
- Use conditional compilation (`#[cfg(target_arch = "wasm32")]`) for platform-specific concurrency code, but minimize the divergence — the core engine logic should be identical.
- Consider `wasm_thread` crate if you absolutely need thread-like abstractions, but prefer restructuring to async.

**Detection:** Any use of `std::thread`, `std::sync::Mutex`, or blocking operations in code that compiles to WASM. Lint for these in CI.

**Confidence:** HIGH — confirmed by Rust WASM documentation and `wasm32-unknown-unknown` platform support docs.

**Phase:** Project scaffolding phase. The dual-target (native + WASM) async boundary must be designed before any engine code.

---

### Pitfall 10: Trigger Ordering and APNAP

**What goes wrong:** When multiple triggered abilities trigger simultaneously, they go on the stack in APNAP (Active Player, Non-Active Player) order. Within a single player's triggers, that player chooses the order. Implementations that ignore APNAP ordering or don't prompt for trigger ordering produce subtly wrong game states.

**Prevention:**
- Implement APNAP ordering from the start: active player's triggers go on stack first (resolving last), then non-active player's.
- Within a single player's triggers, if there are 2+, prompt that player to choose the order (or auto-order when order doesn't matter for AI efficiency).
- Triggers that are "when" vs "whenever" vs "at" have identical mechanical handling — the difference is only in English templating, not timing.
- Track trigger sources carefully — a trigger that says "whenever a creature enters the battlefield" must record which specific creature entered, because the creature might be gone by resolution time (the "last known information" rule).

**Detection:** Test with multiple ETB triggers from the same player. If they always resolve in a fixed order rather than player-chosen, APNAP is broken.

**Confidence:** HIGH — directly from MTG Comprehensive Rules 603.3b.

**Phase:** Trigger system phase. Bake into the trigger architecture, don't bolt on later.

---

### Pitfall 11: Card Image Loading Performance

**What goes wrong:** Loading 7 cards in hand + 20+ permanents on battlefield from Scryfall (even cached) creates a waterfall of HTTP requests on first game. Without progressive loading, the board appears blank for seconds.

**Prevention:**
- Implement a three-tier loading strategy: memory cache → disk cache (IndexedDB for PWA, filesystem for Tauri) → Scryfall API.
- Show card text/stats immediately with a placeholder frame, load images async.
- Pre-fetch images for cards in hand and top of library during opponent's turn.
- Respect Scryfall's rate limits (10 requests/second) — use bulk data download for known sets instead of per-card API calls.
- For WASM/PWA, use service worker caching for card images.

**Detection:** First game after fresh install. If the board takes >2 seconds to show card images, the caching strategy needs work.

**Confidence:** MEDIUM — standard image loading patterns, Scryfall-specific rate limits confirmed in their API docs.

**Phase:** UI phase, specifically when card rendering is implemented.

---

### Pitfall 12: Targeting and Legality Rechecks

**What goes wrong:** Targets are chosen when a spell/ability is put on the stack, but legality is rechecked on resolution. If all targets become illegal, the spell is countered. If some targets become illegal, the spell resolves with remaining legal targets. Implementations that don't recheck (or that recheck incorrectly) allow spells to target dead creatures or fizzle when they shouldn't.

**Prevention:**
- Store target references as IDs, not direct pointers/references. Targets may change zones between targeting and resolution.
- On resolution, re-validate each target independently. Remove illegal targets. If zero remain, counter the spell/ability.
- "Target" vs "choose" is a critical distinction — "choose" doesn't use the targeting system and isn't affected by hexproof/shroud.
- Implement the "last known information" rule: if a target left the battlefield, use its characteristics as they were when it last existed in that zone.

**Detection:** Cast a removal spell targeting a creature, then destroy that creature in response. If the spell still "resolves" instead of being countered, target rechecking is broken.

**Confidence:** HIGH — fundamental MTG rule, common source of bugs in every MTG implementation.

**Phase:** Ability system core phase, when the stack and resolution are implemented.

---

## Minor Pitfalls

---

### Pitfall 13: Webview CSS/Rendering Inconsistencies

**What goes wrong:** Tauri uses the system webview (WKWebView on macOS, WebView2 on Windows, WebKitGTK on Linux). CSS grid layouts, animations, and transforms can behave differently across platforms. A battlefield layout that looks correct on macOS may break on Linux.

**Prevention:**
- Test on all three platforms early, not just your development machine.
- Avoid bleeding-edge CSS features. Stick to well-supported Grid/Flexbox.
- For animations, use Framer Motion or similar libraries that handle cross-browser differences.
- Set a minimum webview version requirement and document it.

**Detection:** CI that runs visual regression tests across platforms (or at minimum, manual testing on Windows + Linux when macOS is the primary dev platform).

**Confidence:** MEDIUM — known Tauri limitation, severity depends on how complex the UI CSS is.

**Phase:** UI phase. Test cross-platform early.

---

### Pitfall 14: Mana Payment Complexity

**What goes wrong:** Mana payment seems simple until hybrid mana ({W/U}), phyrexian mana ({W/P}), generic mana, colorless-only mana ({C}), snow mana, and convoke/delve alternative costs interact. Auto-pay algorithms that work for simple costs break on complex costs.

**Prevention:**
- Implement mana payment as constraint satisfaction, not greedy assignment. Greedy algorithms (pay colored first, then generic) fail when hybrid costs create multiple valid payment paths.
- Always allow manual mana payment as a fallback — players sometimes want to pay a specific way (e.g., leaving blue mana open for a counterspell).
- Handle mana abilities specially: they don't use the stack, resolve immediately, and can be activated during mana payment. This creates a recursive payment loop that must terminate.

**Detection:** Test with hybrid mana costs, triple-color costs with limited lands, and convoke. If auto-pay picks the wrong lands, the algorithm is too greedy.

**Confidence:** HIGH — every MTG engine struggles with this. Forge has extensive mana payment logic.

**Phase:** Core game state engine (mana pool basics) and ability system (mana abilities).

---

### Pitfall 15: Serde Serialization Overhead at WASM Boundary

**What goes wrong:** Using `serde` to serialize/deserialize game state between Rust WASM and JavaScript on every state update creates measurable overhead. With MTG's large state, this can add 5-15ms per serialization round-trip.

**Prevention:**
- Don't serialize full game state. Expose the WASM engine as an opaque object with getter methods: `engine.get_player_life(player_id)`, `engine.get_battlefield_cards()`.
- Use `serde_wasm_bindgen` (not JSON-based serde) when serialization is needed — it's significantly smaller and often faster.
- For the Tauri native path, serialization cost is lower but still matters for large payloads — use Tauri v2's raw byte protocol for bulk data.
- Profile serialization cost explicitly — it's easy to miss because it's spread across many small calls.

**Detection:** If WASM-to-JS boundary calls appear in performance profiles, serialization is the likely culprit.

**Confidence:** MEDIUM-HIGH — documented in wasm-bindgen issues and serde-wasm-bindgen benchmarks.

**Phase:** WASM integration phase. Design the JS-WASM API surface to minimize serialization from the start.

---

## Phase-Specific Warnings

| Phase Topic | Likely Pitfall | Mitigation |
|-------------|---------------|------------|
| Project scaffold | WASM threading model not planned (Pitfall 9) | Design async engine interface before writing engine code |
| Card parser | Multi-face card format edge cases (Pitfall 7) | Build test matrix of all card face types from Forge's cardsfolder |
| Core game state | SBA recursive loop (Pitfall 2), ownership model (Pitfall 4) | Atomic batch SBAs, command buffer pattern for mutations |
| Ability system | Target legality rechecks (Pitfall 12), mana payment (Pitfall 14) | Store target IDs not references, constraint-based mana payment |
| Trigger system | APNAP ordering (Pitfall 10) | Bake into trigger architecture from day one |
| Replacement effects | Self-application loops (Pitfall 3) | Per-event application tracking with unique event IDs |
| Static abilities / layers | Dependency cycles (Pitfall 1) | Port Forge's layer tests first, dependency detection pass |
| AI foundation | Game tree explosion (Pitfall 8) | Heuristic-first, no deep tree search |
| React UI | Card image loading (Pitfall 11), cross-platform CSS (Pitfall 13) | Three-tier cache, test on all platforms early |
| Tauri/WASM integration | IPC bottleneck (Pitfall 6), WASM binary size (Pitfall 5), serde overhead (Pitfall 15) | Delta updates, size budget in CI, opaque handle API |

---

## Sources

- [MTG Layer System Wiki](https://mtg.fandom.com/wiki/Layer) — Layer 613 rules reference
- [State-Based Actions Wiki](https://mtg.fandom.com/wiki/State-based_action) — SBA timing and checking rules
- [Replacement Effect Wiki](https://mtg.fandom.com/wiki/Replacement_effect) — Self-application rules
- [MTG Timing and Priority Wiki](https://mtg.fandom.com/wiki/Timing_and_priority) — Priority system reference
- [Forge Rules Engine Blog](http://mtgrares.blogspot.com/2009/12/rules-engine-is-pain-in-neck.html) — Original Forge developer on rules engine complexity
- [Shrinking WASM Size - Rust Book](https://rustwasm.github.io/book/reference/code-size.html) — Official Rust WASM size optimization guide
- [WASM Size Diet (Medium)](https://medium.com/beyond-localhost/wasm-size-diet-rust-binaries-under-one-megabyte-9104c1bc30b2) — Practical WASM size reduction techniques
- [Leptos Binary Size Optimization](https://book.leptos.dev/deployment/binary_size.html) — Build profile configuration for WASM
- [Tauri IPC Discussion #5690](https://github.com/tauri-apps/tauri/discussions/5690) — IPC performance improvements in Tauri v2
- [Tauri v2 Performance Guide (Oflight)](https://www.oflight.co.jp/en/columns/tauri-v2-performance-bundle-size) — Tauri optimization strategies
- [Avoiding Serde in Rust WASM (Medium)](https://medium.com/@wl1508/avoiding-using-serde-and-deserde-in-rust-webassembly-c1e4640970ca) — Serialization alternatives
- [wasm-bindgen Serde Guide](https://rustwasm.github.io/docs/wasm-bindgen/reference/arbitrary-data-with-serde.html) — Official serde-wasm-bindgen docs
- [wasm32-unknown-unknown Platform Support](https://doc.rust-lang.org/beta/rustc/platform-support/wasm32-unknown-unknown.html) — Threading limitations
- [wasm_thread crate](https://github.com/chemicstry/wasm_thread) — WASM threading workarounds
- [min-sized-rust (GitHub)](https://github.com/johnthagen/min-sized-rust) — Comprehensive Rust binary size reduction guide
- [Forge GitHub Repository](https://github.com/Card-Forge/forge) — Source reference for card format and architecture
