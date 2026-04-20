# Nom Combinator Patterns — Copy-Paste Reference

**This document exists because writing nom combinators correctly the first time
is faster than writing string-matching code and refactoring it after review.**
When the parser combinator gate (`scripts/check-parser-combinators.sh`) flags
your diff, find the matching pattern below and use it directly.

The CLAUDE.md mandate: **all parsing dispatch must use nom combinators**. No
`.strip_prefix(...)`, `.strip_suffix(...)`, `.contains("...")`, `.starts_with("...")`,
`.ends_with("...")`, `.split_once(...)`, `.find("...")`, or `.trim_end_matches("...")`
on string literals in `crates/engine/src/parser/`. Existing offenders are frozen
in amber via the diff-based gate; new code uses these patterns instead.

All examples assume the standard import set:

```rust
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until, take_while1};
use nom::character::complete::{multispace0, multispace1};
use nom::combinator::{map, opt, recognize, value};
use nom::multi::{many0, many1, separated_list1};
use nom::sequence::{delimited, pair, preceded, terminated, tuple};
use nom::Parser;
```

For Oracle text (mixed-case input), prefer the bridge helpers in
`oracle_nom/bridge.rs`: `nom_on_lower`, `nom_on_lower_required`. For atomic
parsing of numbers, mana, counters, etc., use `oracle_nom/primitives.rs`.

---

## Pattern Index

1. [Strip optional fixed prefix](#1-strip-optional-fixed-prefix)
2. [Strip optional fixed suffix](#2-strip-optional-fixed-suffix)
3. [Optional trailing clause after a token sequence](#3-optional-trailing-clause-after-a-token-sequence)
4. [Match one of N alternatives (literal dispatch)](#4-match-one-of-n-alternatives-literal-dispatch)
5. [Word-boundary scan for a phrase that may appear anywhere](#5-word-boundary-scan-for-a-phrase-that-may-appear-anywhere)
6. [Split on a delimiter (was: `split_once`)](#6-split-on-a-delimiter-was-split_once)
7. [Check whether a pattern is present (was: `.contains("…")`)](#7-check-whether-a-pattern-is-present-was-contains)
8. [Composing alt() — branch by shared prefix](#8-composing-alt--branch-by-shared-prefix)
9. [When to use the allow-noncombinator escape hatch](#9-when-to-use-the-allow-noncombinator-escape-hatch)

---

## 1. Strip optional fixed prefix

**Was**:
```rust
let body = input.strip_prefix("at the beginning of ").unwrap_or(input);
```

**Use**:
```rust
let (body, _) = opt(tag("at the beginning of ")).parse(input)?;
// body is the remainder; the `_` is Option<&str> indicating presence.
```

If you need to know whether the prefix was present, capture the option:
```rust
let (body, prefix) = opt(tag("at the beginning of ")).parse(input)?;
let was_present = prefix.is_some();
```

---

## 2. Strip optional fixed suffix

Nom is forward-parsing — it has no native "consume from the end". Two clean
forms exist; pick the one that matches your shape.

### 2a. The string is the entire parse — terminate with the suffix

**Was**:
```rust
let body = input.strip_suffix(" until end of turn").unwrap_or(input);
```

**Use** when you're parsing the whole string and the suffix is fixed:
```rust
let (_, body) = terminated(
    take_until(" until end of turn"),
    opt(tag(" until end of turn")),
).parse(input)?;
```

### 2b. The string is consumed by an upstream combinator that halts before the suffix

This is the more common case in this codebase. See pattern 3.

---

## 3. Optional trailing clause after a token sequence

**Canonical example**: parsing `"Horror enchantment creature in addition to its other types"`.
The type-token sequence (`Horror enchantment creature`) halts naturally at
`in` (not a known type). The trailing clause is then optionally consumed.

**Was** (forbidden):
```rust
let trimmed = descriptor
    .strip_suffix(" in addition to its other types")
    .or_else(|| descriptor.strip_suffix(" in addition to its other creature types"))
    .unwrap_or(descriptor);
let tokens = parse_animation_type_sequence(trimmed)?;
```

**Use**:
```rust
let mut parser = (
    parse_animation_type_sequence,
    opt(preceded(
        multispace0,
        // Try the LONGER alternative first — alt() is short-circuit.
        alt((
            tag("in addition to its other creature types"),
            tag("in addition to its other types"),
        )),
    )),
);
let (_, (tokens, _trailer)) = parser.parse(descriptor)?;
```

**Critical**: in `alt()`, longer alternatives that share a prefix MUST come
first. `tag("foo bar")` will never match if `tag("foo")` precedes it.

---

## 4. Match one of N alternatives (literal dispatch)

**Was**:
```rust
if input == "destroy" || input == "exile" || input == "sacrifice" {
    ...
}
```

**Use** for parsing dispatch:
```rust
let (rest, verb) = alt((
    value(Verb::Destroy, tag("destroy")),
    value(Verb::Exile, tag("exile")),
    value(Verb::Sacrifice, tag("sacrifice")),
)).parse(input)?;
```

`value(X, parser)` runs `parser` and returns `X` on success — perfect for
mapping literal phrases to typed enum arms.

---

## 5. Word-boundary scan for a phrase that may appear anywhere

When a phrase like a timing restriction can appear at any position in the
string (not just the start), and you need to find it without committing to
a position.

**Was** (forbidden):
```rust
while let Some(pos) = remaining.find("during your upkeep") { ... }
```

**Use** the codebase's established pattern (see `scan_timing_restrictions`
in `oracle_casting.rs` and `scan_for_phase` in `oracle_trigger.rs`):

```rust
fn scan_for_clause<'a, F, T>(
    mut remaining: &'a str,
    mut combinator: F,
) -> Vec<T>
where
    F: FnMut(&'a str) -> nom::IResult<&'a str, T>,
{
    let mut results = Vec::new();
    while !remaining.is_empty() {
        if let Ok((rest, val)) = combinator(remaining) {
            results.push(val);
            remaining = rest.trim_start();
        } else {
            // Advance to the next word boundary.
            // allow-noncombinator: word-boundary skip, not parsing dispatch.
            remaining = match remaining.find(' ') {
                Some(i) => remaining[i + 1..].trim_start(),
                None => "",
            };
        }
    }
    results
}
```

This idiom is the single allowed use of `.find(' ')` — it's word-boundary
advancement, not pattern matching. Annotate the line.

---

## 6. Split on a delimiter (was: `split_once`)

**Was**:
```rust
let (before, after) = input.split_once(" — ")?;
```

**Use**:
```rust
let (after, before) = terminated(
    take_until(" — "),
    tag(" — "),
).parse(input)?;
// Note: `before` is the parsed output, `after` is the remainder.
```

If the delimiter is optional, wrap in `opt`:
```rust
let (rest, parts) = opt((take_until(" — "), tag(" — "))).parse(input)?;
match parts {
    Some((before, _delim)) => /* delimited form */,
    None => /* whole input is `before`, no delimiter present */,
}
```

---

## 7. Check whether a pattern is present (was: `.contains("…")`)

**Avoid this pattern entirely for parsing dispatch.** If you find yourself
asking "does the string contain X", rewrite to "parse forward until I hit
X (if I do)":

**Was** (forbidden):
```rust
if input.contains(" until end of turn") {
    duration = Duration::UntilEndOfTurn;
}
```

**Use**: integrate into the actual parse:
```rust
let (rest, duration) = opt(preceded(
    multispace0,
    value(Duration::UntilEndOfTurn, tag("until end of turn")),
)).parse(remaining_after_effect)?;
```

The `opt` makes the duration optional; absence yields `None`. This is
order-of-magnitude better than a pre-pass `.contains()` because the parse
both *detects* and *consumes* the clause in one step.

---

## 8. Composing alt() — branch by shared prefix

When multiple branches share a prefix (e.g. `"during your upkeep"`,
`"during your end step"`, `"during your turn"`), nest them. Don't expand
the cross product.

**Bad** (cross-product expansion):
```rust
alt((
    tag("during your upkeep"),
    tag("during your end step"),
    tag("during your turn"),
    tag("during an opponent's upkeep"),
    tag("during an opponent's end step"),
    tag("during an opponent's turn"),
))
```

**Good** (nested by prefix):
```rust
preceded(
    tag("during "),
    alt((
        preceded(tag("your "), parse_player_phase_phrase),
        preceded(tag("an opponent's "), parse_opponent_phase_phrase),
    )),
)
```

This mirrors BNF grammar production rules and eliminates redundant prefix
matching. When you find yourself writing `tag("X foo")` and `tag("X bar")`
in the same `alt`, factor `X` into a `preceded`.

---

## 9. When to use the `allow-noncombinator` escape hatch

The gate honors `// allow-noncombinator: <reason>` on a line. **Use it for
genuinely structural string operations, not as a way to avoid writing nom
code.** The reason field is mandatory and should be self-documenting.

Legitimate uses:

- **TextPair dual-string operations**: `TextPair::strip_prefix` is correct
  and required for case-insensitive matching that preserves original casing.
- **Punctuation cleanup on already-chunked input**: e.g. `text.trim_end_matches('.')`
  on the result of `take_until(...)`. Trim with a `char` argument is fine
  (the gate only flags double-quoted string args).
- **Already-tokenized input**: if upstream code already split the input by
  `"` or by sentence, downstream cleanup of those chunks with `strip_*` /
  `trim_end_matches` may be structural.
- **Word-boundary scanning** (pattern 5): a single `.find(' ')` per loop
  iteration to advance to the next token start.

Illegitimate uses (these will be rejected at review even with the
annotation):

- "I'll convert it to combinators later" — there is no later.
- "It's only one card" — every parser change must build for the class.
- "The combinator version is harder to read" — refactor the combinator
  helper instead. The codebase has 100+ existing combinators in
  `oracle_nom/` to model on.

---

## When a new pattern doesn't fit any of the above

1. **Trace an analogous existing combinator.** Grep `oracle_nom/`,
   `oracle_trigger.rs`, `oracle_static.rs`, `oracle_effect/` for similar
   parse shapes. The codebase has hundreds; one almost certainly matches.
2. **Read CLAUDE.md** — the "Rust Idioms" section has the canonical guidance.
3. **Add a new pattern entry to this file** in your PR if you introduce a
   genuinely novel composition. PATTERNS.md is meant to grow — the goal is
   that the next agent who hits the same shape finds the answer here
   instead of reinventing it.
