---
name: vercel-react-best-practices
description: React performance optimization guidelines, adapted for the phase.rs Vite+React+Zustand+Tauri frontend. Use when writing, reviewing, or refactoring React code in client/src/ to ensure optimal performance patterns. Triggers on React component work, data-flow refactoring, bundle optimization, or perf reviews of the client.
license: MIT
metadata:
  upstream: vercel/vercel-react-best-practices
  upstream-version: "1.0.0"
  adapted-for: phase.rs (Vite, no Next.js, no SSR)
---

# Vercel React Best Practices (phase.rs-adapted)

Performance-rules reference for the phase.rs React frontend. Forked from Vercel's
official guide and trimmed to the 36 rules that apply to a Vite + React +
Zustand + Tauri stack (Next.js server components, SWR, hydration-flicker, and
Next-route-specific async rules are removed).

## When to apply

Reference these rules when:

- Writing new React components in `client/src/components/**`
- Refactoring Zustand-subscribing components for re-render performance
- Investigating performance issues in the board, hand zone, stack, or animations
- Reviewing frontend changes — `review-impl`'s Frontend lens cites this skill
- Optimizing bundle size for the Vite build, the Tauri shell, or the PWA

## Phase.rs-specific notes

- **No Next.js.** All `next/dynamic` examples have been rewritten to use
  `React.lazy()` + `Suspense`. Server-component rules were dropped entirely.
- **No SSR / no hydration.** SSR-only rules (`rendering-hydration-no-flicker`)
  were dropped.
- **No SWR.** Phase.rs reads game state from a synchronous Zustand store fed by
  the engine adapter — SWR's deduplication isn't a concern here.
- **Animation-heavy.** Framer Motion + Tailwind v4. `rendering-animate-svg-wrapper`,
  `rerender-defer-reads`, and `rerender-memo` are the most directly applicable
  rules in practice.
- **Game-state subscriptions are the hot path.** `gameStore`, `uiStore`, and
  `animationStore` get subscribed all over the board UI — the entire `rerender-*`
  category is worth keeping in mind when adding new selectors.

## Rule categories (priority order)

| Priority | Category | Impact | Filename prefix |
|----------|----------|--------|-----------------|
| 1 | Bundle Size Optimization | CRITICAL | `bundle-` |
| 2 | Eliminating Waterfalls | CRITICAL | `async-` |
| 3 | Re-render Optimization | MEDIUM | `rerender-` |
| 4 | Rendering Performance | MEDIUM | `rendering-` |
| 5 | Client-Side Event Plumbing | MEDIUM | `client-` |
| 6 | JavaScript Performance | LOW-MEDIUM | `js-` |
| 7 | Advanced Patterns | LOW | `advanced-` |

Server-side rules were dropped (no Next.js).

## Quick Reference

### Bundle Size (CRITICAL)

- `bundle-barrel-imports` — Import directly, avoid barrel files
- `bundle-dynamic-imports` — Use `React.lazy()` for heavy components (Monaco, charts, deck-import dialogs)
- `bundle-defer-third-party` — Load analytics/logging after hydration
- `bundle-conditional` — Load modules only when feature is activated
- `bundle-preload` — Preload on hover/focus for perceived speed

### Eliminating Waterfalls (CRITICAL)

- `async-defer-await` — Move `await` into branches where the value is actually used
- `async-parallel` — Use `Promise.all()` for independent operations
- `async-dependencies` — Use `better-all` for partial dependencies

### Re-render Optimization (MEDIUM — hot path for game state)

- `rerender-defer-reads` — Don't subscribe to state only used in callbacks
- `rerender-memo` — Extract expensive work into memoized components
- `rerender-dependencies` — Use primitive dependencies in effects
- `rerender-derived-state` — Subscribe to derived booleans, not raw values
- `rerender-functional-setstate` — Use functional setState for stable callbacks
- `rerender-lazy-state-init` — Pass function to `useState` for expensive values
- `rerender-transitions` — Use `startTransition` for non-urgent updates

### Rendering Performance (MEDIUM)

- `rendering-animate-svg-wrapper` — Animate div wrapper, not SVG element
- `rendering-content-visibility` — Use `content-visibility` for long lists (relevant: graveyard, exile, log)
- `rendering-hoist-jsx` — Extract static JSX outside components
- `rendering-svg-precision` — Reduce SVG coordinate precision (mana symbols, icons)
- `rendering-activity` — Use Activity component for show/hide
- `rendering-conditional-render` — Use ternary, not `&&` for conditionals

### Client-Side Event Plumbing (MEDIUM)

- `client-event-listeners` — Deduplicate global event listeners

### JavaScript Performance (LOW-MEDIUM)

- `js-batch-dom-css` — Group CSS changes via classes or `cssText`
- `js-index-maps` — Build `Map` for repeated lookups
- `js-cache-property-access` — Cache object properties in loops
- `js-cache-function-results` — Cache function results in module-level `Map`
- `js-cache-storage` — Cache `localStorage`/`sessionStorage` reads
- `js-combine-iterations` — Combine multiple `filter`/`map` into one loop
- `js-length-check-first` — Check array length before expensive comparison
- `js-early-exit` — Return early from functions
- `js-hoist-regexp` — Hoist `RegExp` creation outside loops
- `js-min-max-loop` — Use loop for min/max instead of sort
- `js-set-map-lookups` — Use `Set`/`Map` for O(1) lookups
- `js-tosorted-immutable` — Use `toSorted()` for immutability

### Advanced Patterns (LOW)

- `advanced-event-handler-refs` — Store event handlers in refs
- `advanced-use-latest` — `useLatest` for stable callback refs

## How to use

Read individual rule files for explanations and code examples:

```
rules/rerender-defer-reads.md
rules/bundle-barrel-imports.md
rules/js-set-map-lookups.md
```

Each rule file contains the same shape:

- Frontmatter with `impact` and `tags`
- Brief explanation of why it matters
- Incorrect code example
- Correct code example
- Additional context where relevant

## Provenance

Original content © Vercel, licensed MIT. See `https://github.com/vercel/vercel`
for the upstream. Local adaptations:

- 9 rules dropped (`server-*` ×5, `async-api-routes`, `async-suspense-boundaries`,
  `client-swr-dedup`, `rendering-hydration-no-flicker`).
- `bundle-dynamic-imports` and `bundle-defer-third-party` examples rewritten to
  use `React.lazy()` instead of `next/dynamic`.
- This SKILL.md rewritten to reflect the phase.rs-applicable subset and call out
  hot paths in the game UI.
