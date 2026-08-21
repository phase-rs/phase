//! THE GATE: a `WaitingFor` state with no acting player must declare what advances it.
//!
//! Read this module doc in full before changing a pin below — a pin moved without an
//! adjudicated table entry defeats the whole file. NO FIGURE THIS FILE ASSERTS IS
//! RESTATED HERE, not even to narrate why it exists: each number lives exactly once,
//! in the assertion that owns it, where an author editing it has its message in view.
//! A rustdoc restatement of an asserted figure is unanchored by construction and drifts
//! silently, which is the defect class this whole census exists to make loud.
//!
//! The gate rests on ONE structural claim: every arm of `WaitingFor::acting_authority`
//! is an UNGUARDED pattern whose body is exactly one `ActingAuthority::` constructor
//! call — possibly wrapped by `rustfmt` in a block whose single tail expression is that
//! call. `A8` (no guards), `A3a`/`A3b` (ctor-shaped bodies, path-shaped answers) and
//! `A0` (no preprocessing) are what make that claim true rather than merely hoped for.
//!
//! WHY THREE ASSERTIONS AND NOT ONE. A match GUARD is the subtle escape: guards do not
//! count towards exhaustivity, so a guarded "actorless" arm can sit above an unguarded
//! fallback, compile clean, and be silently lost. Three assertions close it, and NONE of
//! them is redundant — this was measured, and an earlier revision of this gate's design
//! got it wrong in a way that would have licensed deleting two of them:
//!
//!   * `A4b` (duplicate) catches it ONLY when the fallback arm NAMES the variant.
//!     A wildcard COVERS a variant without NAMING it, so nothing is inserted twice and
//!     `A4b` is SILENT. `A4b`'s completeness is conditional on `A1`.
//!   * `A8` (no guards) catches the guarded-arm-plus-wildcard shape that `A4b` misses.
//!   * `A1` (no wildcard / no bare binding) is the SOLE net for a GUARD-FREE arm plus a
//!     wildcard actorless fallback: `A8` is silent (no guard), `A4b` is silent (no
//!     second naming arm), and totality passes.
//!
//! Do not delete any of `A1`, `A4b`, `A8` on the grounds that another covers it.
//!
//! Two further directional dependencies, both measured: `A3b` accepts any
//! `NoActor::`-qualified PATH, so an associated const would satisfy it — `A5`, and NOT
//! `A7`, is what rejects the answer such a path records. And the `WaitingFor` / `NoActor` /
//! `ActingAuthority` enum lookups are `.expect`s rather than numbered assertions; a
//! permissive lookup would degrade into `A4a`'s `extra=[..]` side plus the declared-count
//! pin instead of failing where the cause is.
//!
//! `A11` is the only assertion here that reads the BODY of a function other than
//! `acting_authority` (`A9` reads a consumer's existence, never its body):
//! `acting_player` and `acting_players` must each be a single tail match on
//! `self.acting_authority()`. Every other body-reading assertion here reads
//! `acting_authority` while production reads the adapters, so an adapter re-forked
//! into its own match over `WaitingFor` variants is adjudicated by nothing —
//! measured on a fixture, not assumed. `A11`'s reach is the TOP-LEVEL body shape of the
//! two adapters it names: a per-variant match nested inside an arm of the delegating
//! match is not read, and a third adapter over `acting_authority` would be read by
//! nothing here.
//!
//! HOW THIS FILE FAILS. Findings are COLLECTED into a `Vec<String>` and asserted ONCE at
//! the end; only the preconditions (parse, enum lookups, `A10`, `A0`) and the `ACTORLESS`
//! cardinality reach-guard panic immediately. That is deliberate and load-bearing, not a
//! style choice: the shapes this gate catches are diagnosed by the COMBINATION of reds —
//! a guarded actorless arm reds `A8`, `A4b` and `A5` together, and under a fail-fast
//! reading only the first is ever seen, which would make `A4b` look silent on the one
//! fixture that proves it is not. Do not convert these back into individual `assert!`s.
//!
//! Findings are emitted in a stable order on three nested levels: every per-arm finding
//! precedes every whole-census finding; arms are walked in source order; and within one
//! arm the steps run pattern extraction (`A1`/`A2`) first, then the guard check (`A8`),
//! then body classification (`A3a`/`A3b`), then the duplicate check (`A4b`). An
//! implementation that emits `A8` before `A1` for a single guarded wildcard arm is
//! wrong, not merely differently formatted.
//!
//! Pattern follows `loop_shortcut.rs`'s `syn` census: parse the source rather than
//! grep it, so comments and strings cannot be mistaken for code.

use engine::types::game_state::{
    MulliganBottomEntry, MulliganDecisionEntry, MulliganDecisionPhase, OpeningHandBottomReason,
    WaitingFor,
};
use engine::types::player::PlayerId;
use std::collections::{BTreeMap, BTreeSet};
use syn::{Expr, ImplItem, Item, Pat, Stmt};

const GAME_STATE: &str = include_str!("../../src/types/game_state.rs");
const RESOLVE_BATCH: &str = include_str!("../../src/game/engine_resolve_batch.rs");

/// Every production source this census can search FOR A CONSUMER, keyed the way
/// `ACTORLESS` names it. `include_str!` takes a literal path, so THIS TABLE IS
/// THE CENSUS'S CONSUMER REACH: a consumer living anywhere else cannot be looked
/// up, and `A9` says so out loud rather than searching the wrong file and
/// reporting "not found". Note that `GAME_STATE` is `include_str!`ed too but is
/// deliberately NOT in this table — it is the census's *subject*, not a consumer
/// source — so "held by an `include_str!`" and "searchable by `A9`" are different
/// questions and `A9`'s message must not conflate them.
const SOURCES: &[(&str, &str)] = &[("game/engine_resolve_batch.rs", RESOLVE_BATCH)];

/// THE ADJUDICATION TABLE. Every actorless `WaitingFor` variant, the `NoActor`
/// answer its arm names, the (source file, free function) that actually advances
/// it, and prose a reviewer can disagree with.
///
/// The consumer is a PAIR, not a bare name. A bare name is silently searched in
/// whichever source the census happens to hold, so a future row naming a function
/// in a third file would report "renamed or deleted" when the truth is "this
/// census never looked". The file key makes that case a distinct, actionable `A9`
/// finding.
const ACTORLESS: &[(&str, &str, (&str, &str), &str)] = &[
    (
        "GameOver",
        "MatchTeardown",
        // Advanced by `game::match_flow`, a module not a single fn, so `A9`
        // skips the empty pair by design. `A9` is a partial binding: it covers
        // the latch case (the one that actually hung) and not the teardown case.
        ("", ""),
        "CR 104.1: the game has ended. `game::match_flow` reads the winner and tears \
         the game down; nothing inside the game advances it.",
    ),
    (
        "ResolveAllReady",
        "ResolveAllReadyPrefix",
        ("game/engine_resolve_batch.rs", "resolve_all_ready_prefix"),
        "CR 117.4: a consent latch. `game::engine_resolve_batch::resolve_all_ready_prefix` \
         consumes it. EVERY transport able to MINT this latch must also call that \
         consumer — a server-driven AI seat that minted one without a consumer hung a \
         3-player Commander game permanently.",
    ),
];

/// The two variants CR 103.5 lets decide simultaneously. Pinned for the same reason
/// as `ACTORLESS`: `Simultaneous` must not become an unadjudicated escape hatch for a
/// state that in fact has nobody to act.
const SIMULTANEOUS: &[&str] = &["MulliganDecision", "OpeningHandBottomCards"];

/// The complete `ActingAuthority` constructor vocabulary this census reads. `A4c`
/// rejects every other name, which is what makes the `class` lookups below a TOTAL
/// reading of the match rather than a filtered one: a further constructor — a
/// new enum variant, or an associated `fn` on the type — would otherwise route its
/// arms past `A5`, `A6` AND `A7` at once, leaving only the declared-count pin to
/// notice, and that pin's own repair text invites bumping the number.
///
/// Hand-written, and `A4d` is what keeps it honest: it compares this list against the
/// enum's declared variants, the same pairing `A7` gives `ACTORLESS`. Neither assertion
/// covers the other — `A4c` alone misses a declared variant no arm has answered yet, and
/// `A4d` alone misses an associated `fn`, which is a constructor path with no variant
/// behind it. Both were measured. `A4c` reads call-shaped bodies only: an arm answering
/// with an associated `const` is a path rather than a call, so `A3a` rejects it and
/// `class` is never written for that arm — the `const` escape is `A3a`'s catch, not
/// `A4c`'s.
///
/// A newly DECLARED variant meets `rustc` before it meets this census: `acting_player`
/// and `acting_players` match `ActingAuthority` exhaustively with no wildcard, so a
/// fourth variant is E0004 in both adapters and nothing runs until arms exist there.
/// `A4d` is what catches it once they do.
///
/// Adding a name here is NOT, on its own, the repair for either red. A new authority
/// class is a question this census does not yet ask, and it needs its own whole-census
/// assertion naming which `WaitingFor` variants may answer it — exactly as `A5` does
/// for the actorless class and `A6` for the simultaneous one.
const AUTHORITIES: &[&str] = &["None", "One", "Simultaneous"];

/// The rest of `A8`'s message. It is long on purpose: an assertion whose only stated
/// rationale is "this shape is dangerous" invites the reader to delete it, so the
/// message names both sanctioned destinations for the condition a guard would have
/// expressed. THE CORRECT RESPONSE TO AN `A8` RED IS NEVER "DELETE `A8`".
const GUARD_ADVICE: &str = "\n\n\
     Guards do not count towards exhaustivity, so a guarded arm can declare an actorless \
     answer above an unguarded fallback, compile clean, and be silently lost by this \
     census. That is the exact shape that hung a 3-player Commander game.\n\n\
     Do NOT delete this assertion, and do NOT assume `A4b` covers it: `A4b` only fires \
     when a later arm NAMES the same variant, and a wildcard fallback covers without \
     naming.\n\n\
     A guard is never the only way to say what you mean:\n\
     \x20 * if the condition changes WHO ACTS, the two cases are different states — add a \
     `WaitingFor` variant. That addition is the counted event this gate exists to make \
     loud, and it is the modelling-correct fix.\n\
     \x20 * if the condition merely narrows an authority the arm already declares, put it \
     in `acting_player` / `acting_players`, exactly as the mulligan `pending.len() == 1` \
     test does.";

/// Every item in `items`, INCLUDING the ones nested in inline `mod` blocks — a
/// declaration does not stop being a declaration for living inside a module.
fn flatten<'a>(items: &'a [Item], out: &mut Vec<&'a Item>) {
    for item in items {
        out.push(item);
        if let Item::Mod(m) = item {
            if let Some((_, inner)) = &m.content {
                flatten(inner, out);
            }
        }
    }
}

/// Variant names of `enum_name`, in declaration order, or `None` if the enum is not
/// declared in `items`. The caller `.expect`s: a permissive lookup here would let a
/// moved enum degrade into a confusing totality failure instead of naming the cause.
fn enum_variants(items: &[&Item], enum_name: &str) -> Option<Vec<String>> {
    items.iter().find_map(|item| match item {
        Item::Enum(e) if e.ident == enum_name => {
            Some(e.variants.iter().map(|v| v.ident.to_string()).collect())
        }
        _ => None,
    })
}

/// Inherent (`trait_.is_none()`) methods named `fn_name` on `self_ty`. Returns every
/// match so the caller can reject BOTH zero (renamed or moved) and two (a second inherent
/// impl block whose self-type's LAST path segment is also `self_ty` — a `cfg`-gated
/// duplicate, or a same-named type in an inline `mod`, which `flatten` descends into).
/// A trait impl cannot produce a two: `trait_.is_some()` is skipped below.
fn inherent_fns<'a>(items: &[&'a Item], self_ty: &str, fn_name: &str) -> Vec<&'a syn::ImplItemFn> {
    let mut found = Vec::new();
    for item in items {
        let Item::Impl(im) = item else { continue };
        if im.trait_.is_some() {
            continue;
        }
        let syn::Type::Path(tp) = im.self_ty.as_ref() else {
            continue;
        };
        if !tp.path.segments.last().is_some_and(|s| s.ident == self_ty) {
            continue;
        }
        for member in &im.items {
            if let ImplItem::Fn(f) = member {
                if f.sig.ident == fn_name {
                    found.push(f);
                }
            }
        }
    }
    found
}

/// Whether `items` declares a free `fn` named `fn_name`.
fn has_free_fn(items: &[&Item], fn_name: &str) -> bool {
    items
        .iter()
        .any(|item| matches!(item, Item::Fn(f) if f.sig.ident == fn_name))
}

/// A human name for an expression's SHAPE.
///
/// `syn`'s AST types implement `Debug` only under the `extra-traits` feature, which
/// this crate's dev-dependency does not enable, and `Span::start()` needs
/// `span-locations`. So neither `{:?}` on a node nor a source line number is
/// available to a failure message here, and a shape literal is what remains.
fn shape(expr: &Expr) -> &'static str {
    match expr {
        Expr::Block(_) => "a block",
        Expr::Unsafe(_) => "an unsafe block",
        Expr::Const(_) => "a const block",
        Expr::If(_) => "an `if`",
        Expr::Match(_) => "a `match`",
        Expr::Call(_) => "a call",
        Expr::MethodCall(_) => "a method call",
        Expr::Macro(_) => "a macro invocation",
        Expr::Path(_) => "a path",
        Expr::Paren(_) => "a parenthesised expression",
        Expr::Struct(_) => "a struct literal",
        _ => "an unsupported expression",
    }
}

/// A human name for a pattern's SHAPE, for the same reason `shape` exists.
fn pattern_shape(pat: &Pat) -> &'static str {
    match pat {
        Pat::Tuple(_) => "a tuple pattern",
        Pat::Slice(_) => "a slice pattern",
        Pat::Lit(_) => "a literal pattern",
        Pat::Range(_) => "a range pattern",
        Pat::Macro(_) => "a macro pattern",
        Pat::Rest(_) => "a rest pattern",
        Pat::Type(_) => "a type-ascribed pattern",
        _ => "an unsupported pattern",
    }
}

/// `rustfmt` block-wraps a match arm whose body exceeds `max_width` (100 by default;
/// the repo has no `rustfmt.toml`). That wrapper is pure formatting and carries no
/// meaning, so it is stripped before classification.
///
/// DELIBERATELY NARROW — each restriction is load-bearing:
///   * exactly ONE statement, and it must be `Stmt::Expr(e, None)` — a tail expression
///     with NO semicolon. A `let`, a semicolon-terminated statement, an item, or any
///     second statement means the answer is computed somewhere this census cannot
///     read, which is precisely what `A3a` exists to reject.
///   * NON-RECURSIVE. One level only. `{ { ctor } }` is not something rustfmt emits,
///     so it stays an `A3a` failure (fail closed).
///   * attrs and labels reject. `#[cfg(..)] { .. }` or `'a: { .. }` are authored
///     constructs, not formatting.
///
/// Stripping the wrapper does NOT weaken `A3a`: the ctor requirement is then applied
/// to whatever was inside, so an `if`, a helper call or a `todo!()` hidden in a block
/// still REDs. It is called ONCE, before the whole `A3a` → `A3b` chain — siting it
/// inside `A3a` alone would leave `A3b` reading the original block, and the arm's
/// answer would never be recorded at all, turning a red into a silent misclassification.
fn unwrap_rustfmt_block(expr: &Expr) -> &Expr {
    let Expr::Block(b) = expr else { return expr };
    if !b.attrs.is_empty() || b.label.is_some() {
        return expr;
    }
    match b.block.stmts.as_slice() {
        [Stmt::Expr(inner, None)] => inner,
        _ => expr,
    }
}

/// Why an arm's pattern yielded no `WaitingFor` variant names.
enum PatternRejection {
    /// `A1` — `_ =>`.
    Wildcard,
    /// `A1` — `other =>`, a bare binding that covers every remaining variant.
    BareBinding(String),
    /// `A2` — a pattern form this census does not recognise, or one qualified by a
    /// path that is not `WaitingFor` / `Self`.
    Unsupported(&'static str),
}

/// The `WaitingFor` variant names an arm pattern covers, flattening `|` alternatives.
///
/// Requires the second-to-last path segment to be `WaitingFor` or `Self`, so a
/// foreign path cannot smuggle an ident in. Every failure is RETURNED, never silently
/// skipped: an arm whose pattern this census cannot read is the exact shape through
/// which an actorless answer escapes.
fn extract_variants(pat: &Pat, out: &mut Vec<String>) -> Result<(), PatternRejection> {
    match pat {
        Pat::Or(or) => or
            .cases
            .iter()
            .try_for_each(|case| extract_variants(case, out)),
        Pat::Reference(r) => extract_variants(&r.pat, out),
        Pat::Paren(p) => extract_variants(&p.pat, out),
        Pat::Struct(s) => push_qualified_variant(&s.path, out),
        Pat::TupleStruct(t) => push_qualified_variant(&t.path, out),
        Pat::Path(p) => push_qualified_variant(&p.path, out),
        Pat::Wild(_) => Err(PatternRejection::Wildcard),
        Pat::Ident(i) => match &i.subpat {
            Some((_, inner)) => extract_variants(inner, out),
            None => Err(PatternRejection::BareBinding(i.ident.to_string())),
        },
        other => Err(PatternRejection::Unsupported(pattern_shape(other))),
    }
}

fn push_qualified_variant(path: &syn::Path, out: &mut Vec<String>) -> Result<(), PatternRejection> {
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    let qualified = segments.len() >= 2
        && matches!(segments[segments.len() - 2].as_str(), "WaitingFor" | "Self");
    if !qualified {
        return Err(PatternRejection::Unsupported(
            "a pattern that is not `WaitingFor::`-qualified",
        ));
    }
    out.push(segments[segments.len() - 1].clone());
    Ok(())
}

/// The `(constructor, NoActor answer)` an arm body declares, or the finding it earns.
///
/// `A3a` is the ctor-shape gate and is the SOLE shape gate on `One` / `Simultaneous`
/// bodies. `A3b` additionally requires `None`'s single argument to be a
/// `NoActor::`-qualified PATH — a computed answer (`None(pick_reason())`) is
/// unreadable by a source census, so it must fail rather than be skipped.
fn classify_body(expr: &Expr) -> Result<(String, Option<String>), String> {
    let expr = unwrap_rustfmt_block(expr);
    let Expr::Call(call) = expr else {
        return Err(format!("A3a arm body is {}", shape(expr)));
    };
    let Expr::Path(func) = call.func.as_ref() else {
        return Err(format!(
            "A3a arm body's callee is {}, not an `ActingAuthority::` constructor path",
            shape(call.func.as_ref())
        ));
    };
    let segments: Vec<String> = func
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    let ctor = segments[segments.len() - 1].clone();
    let on_authority = segments.len() >= 2 && segments[segments.len() - 2] == "ActingAuthority";
    if !on_authority {
        return Err(format!("A3a ctor is not ActingAuthority::*: `{ctor}`"));
    }
    if ctor != "None" {
        return Ok((ctor, None));
    }
    if call.args.len() != 1 {
        return Err(format!(
            "A3b ActingAuthority::None takes exactly one NoActor:: path argument, found {}",
            call.args.len()
        ));
    }
    let arg = call
        .args
        .first()
        .expect("exactly one argument, just checked");
    let Expr::Path(answer) = arg else {
        return Err(format!(
            "A3b the NoActor answer is {}, not a NoActor:: path",
            shape(arg)
        ));
    };
    let answer_segments: Vec<String> = answer
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    let qualified =
        answer_segments.len() >= 2 && answer_segments[answer_segments.len() - 2] == "NoActor";
    if !qualified {
        return Err(format!(
            "A3b the NoActor answer `{}` is not a NoActor:: path",
            answer_segments.join("::")
        ));
    }
    Ok((
        ctor,
        Some(answer_segments[answer_segments.len() - 1].clone()),
    ))
}

/// Why an adapter's body is not a delegation to `acting_authority` — `A11`'s whole
/// predicate — or `Ok(())`.
///
/// Narrow for `A0`'s reason: a body this census cannot read is a body that could be
/// deciding an authority instead of narrowing one. A legitimate restructuring REDs and
/// gets adjudicated here; that red is the point, not a cost.
fn adapter_delegation(f: &syn::ImplItemFn) -> Result<(), String> {
    let [Stmt::Expr(Expr::Match(m), None)] = f.block.stmts.as_slice() else {
        return Err("its body is not a single tail `match` expression".to_string());
    };
    let scrutinee = m.expr.as_ref();
    let Expr::MethodCall(call) = scrutinee else {
        let saw = if matches!(scrutinee, Expr::Path(p) if p.path.is_ident("self")) {
            "`self`"
        } else {
            shape(scrutinee)
        };
        return Err(format!("it matches on {saw}"));
    };
    let on_self = matches!(call.receiver.as_ref(), Expr::Path(p) if p.path.is_ident("self"));
    if !on_self || call.method != "acting_authority" || !call.args.is_empty() {
        return Err(format!(
            "it matches on a `.{}()` call that is not the no-argument `self.acting_authority()`",
            call.method
        ));
    }
    Ok(())
}

fn render_set(items: &BTreeSet<String>) -> String {
    let joined: Vec<&str> = items.iter().map(String::as_str).collect();
    format!("{{{}}}", joined.join(", "))
}

fn render_strs(items: &[&str]) -> String {
    format!("[{}]", items.join(", "))
}

#[test]
fn every_waiting_for_arm_declares_its_acting_authority() {
    // ---- Phase 0.1 — parse. A failure here makes every later finding noise. ----
    let game_state = syn::parse_file(GAME_STATE).expect("parse `types/game_state.rs`");
    let mut game_state_items = Vec::new();
    flatten(&game_state.items, &mut game_state_items);

    let parsed_sources: Vec<(&str, syn::File)> = SOURCES
        .iter()
        .map(|(key, src)| {
            (
                *key,
                syn::parse_file(src).unwrap_or_else(|e| panic!("parse `{key}`: {e}")),
            )
        })
        .collect();
    let source_items: Vec<(&str, Vec<&Item>)> = parsed_sources
        .iter()
        .map(|(key, parsed)| {
            let mut items = Vec::new();
            flatten(&parsed.items, &mut items);
            (*key, items)
        })
        .collect();

    // ---- Phase 0.2 — enum lookups. `.expect`, never a permissive default. ----
    let declared = enum_variants(&game_state_items, "WaitingFor")
        .expect("`WaitingFor` must be declared in `types/game_state.rs`");
    let no_actor_variants = enum_variants(&game_state_items, "NoActor")
        .expect("`NoActor` must be declared in `types/game_state.rs`");
    let authority_variants = enum_variants(&game_state_items, "ActingAuthority")
        .expect("`ActingAuthority` must be declared in `types/game_state.rs`");

    // ---- Phase 0.3 — A10: exactly one inherent `WaitingFor::acting_authority`. ----
    let found = inherent_fns(&game_state_items, "WaitingFor", "acting_authority");
    assert_eq!(
        found.len(),
        1,
        "A10: expected exactly one inherent `WaitingFor::acting_authority`, found {}. \
         Zero means it was renamed or moved and this census now asserts nothing at all; \
         two means a second inherent impl block whose self-type's last path segment is \
         also `WaitingFor` — a `cfg`-gated duplicate, or a same-named type in an inline \
         `mod` — and the census may be reading the wrong one. A trait impl is skipped and \
         cannot be the cause.",
        found.len()
    );
    let acting_authority = found[0];

    // ---- Phase 0.4 — A0: the body is a single tail `match`, with no preprocessing. ----
    let arms = match acting_authority.block.stmts.as_slice() {
        [Stmt::Expr(Expr::Match(m), None)] => &m.arms,
        _ => panic!(
            "A0: `WaitingFor::acting_authority`'s body must be exactly one tail `match self` \
             expression — no `let`, no early return, no preprocessing. Anything computed \
             before the arms is an answer this census cannot read."
        ),
    };

    // ---- Phase 1 — per arm, in source order. Everything is pushed; nothing panics. ----
    let mut failures: Vec<String> = Vec::new();
    let mut classified: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
    let mut class: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for arm in arms {
        // Step 1 — extract the pattern, so every later message can NAME this arm.
        let mut variants = Vec::new();
        let extraction = extract_variants(&arm.pat, &mut variants);
        let arm_label = match &extraction {
            Ok(()) => format!("arm covering `{}`", variants.join(", ")),
            Err(PatternRejection::Wildcard) => "the wildcard/catch-all arm".to_string(),
            Err(PatternRejection::BareBinding(name)) => format!("arm binding `{name}`"),
            Err(PatternRejection::Unsupported(what)) => format!("the arm whose pattern is {what}"),
        };
        match &extraction {
            Ok(()) => {}
            Err(PatternRejection::Wildcard) => failures.push(
                "A1 arm pattern rejected: the wildcard arm. A `_ =>` fallback covers a variant \
                 WITHOUT naming it, so `A4b` cannot see it and the answer it gives is \
                 unadjudicated. Name every variant explicitly."
                    .to_string(),
            ),
            Err(PatternRejection::BareBinding(name)) => failures.push(format!(
                "A1 arm pattern rejected: bare binding `{name}`. A bare binding is a wildcard \
                 with a name — it covers every remaining variant without naming any."
            )),
            Err(PatternRejection::Unsupported(what)) => failures.push(format!(
                "A2 arm pattern rejected: {what}. Every arm pattern must resolve to at least \
                 one `WaitingFor::`-qualified variant name."
            )),
        }

        // Step 2 — A8, on EVERY arm including one whose extraction failed: a guarded
        // wildcard is the escape shape, and A8's message is the actionable one.
        if arm.guard.is_some() {
            failures.push(format!(
                "A8 {arm_label} of `WaitingFor::acting_authority` carries a match guard.{GUARD_ADVICE}"
            ));
        }

        // An arm whose pattern this census could not read has no variant name to key the
        // map on, so classification is skipped — the finding is already recorded.
        if extraction.is_err() {
            continue;
        }

        // Step 3 — A3a / A3b.
        let answer = match classify_body(&arm.body) {
            Ok(answer) => answer,
            Err(finding) => {
                failures.push(format!("{finding} ({arm_label})"));
                continue;
            }
        };

        for variant in &variants {
            // Step 4 — A4b, on each map write.
            if let Some(previous) = classified.insert(variant.clone(), answer.clone()) {
                failures.push(format!(
                    "A4b variant `{variant}` classified twice: {previous:?} then {answer:?}. \
                     Two arms name the same variant, so one of the two answers is dead — and \
                     if the dead one is the actorless declaration, the state's advancer is \
                     silently lost."
                ));
            }
            // Step 5 — `class` is ACCUMULATED per arm, never rebuilt from `classified`.
            // A source census must report what the source DECLARES, not what `rustc` would
            // select: the derived reading silently discards an actorless declaration that a
            // later arm overwrites, which is the exact silent-overwrite shape this gate exists
            // to catch.
            class
                .entry(answer.0.clone())
                .or_default()
                .insert(variant.clone());
        }
    }

    // ---- Phase 2 — whole-census findings, in assertion-number order. ----
    assert_eq!(
        ACTORLESS.len(),
        2,
        "reach-guard: the adjudication table in this file lost a row"
    );

    let declared_set: BTreeSet<String> = declared.iter().cloned().collect();
    let classified_set: BTreeSet<String> = classified.keys().cloned().collect();
    let missing: Vec<&String> = declared_set.difference(&classified_set).collect();
    let extra: Vec<&String> = classified_set.difference(&declared_set).collect();
    if !missing.is_empty() || !extra.is_empty() {
        failures.push(format!(
            "A4a totality missing={missing:?} extra={extra:?}. Every `WaitingFor` variant must \
             be classified by exactly one arm of `acting_authority`."
        ));
    }

    for (ctor, variants) in &class {
        if !AUTHORITIES.contains(&ctor.as_str()) {
            failures.push(format!(
                "A4c arms {} answer `ActingAuthority::{ctor}`, a constructor this census does \
                 not read: it reads exactly {}, and `A5`, `A6` and `A7` each interrogate one of \
                 those. Do NOT repair this by adding the name to `AUTHORITIES` — read that \
                 constant's doc first.",
                render_set(variants),
                render_strs(AUTHORITIES),
            ));
        }
    }

    let authority_declared: BTreeSet<String> = authority_variants.iter().cloned().collect();
    let authority_pinned: BTreeSet<String> = AUTHORITIES.iter().map(|s| (*s).to_string()).collect();
    if authority_declared != authority_pinned {
        failures.push(format!(
            "A4d `ActingAuthority` declares {} but `AUTHORITIES` lists {}. `AUTHORITIES` is what \
             `A4c` reads the arms against, so without this comparison it is a hand-copied \
             restatement of an enum with nothing holding the two in step. A variant declared and \
             not listed is a class `A4c` would reject the moment an arm answered it; one listed \
             and not declared is vocabulary no arm can name.",
            render_set(&authority_declared),
            render_set(&authority_pinned),
        ));
    }

    let table_answers: BTreeMap<String, String> = ACTORLESS
        .iter()
        .map(|(variant, answer, _, _)| ((*variant).to_string(), (*answer).to_string()))
        .collect();
    let empty = BTreeSet::new();
    let actorless_class = class.get("None").unwrap_or(&empty);
    let table_variants: BTreeSet<String> = table_answers.keys().cloned().collect();
    if actorless_class != &table_variants {
        failures.push(format!(
            "A5 actorless class {} != table {}. A variant whose arm answers \
             `ActingAuthority::None` must appear in `ACTORLESS` with the answer its arm names, \
             and prose a reviewer can disagree with. If your new variant has no acting player, \
             adjudicate it here — do not reclassify it to make this green.",
            render_set(actorless_class),
            render_set(&table_variants),
        ));
    }
    for variant in actorless_class.intersection(&table_variants) {
        let recorded = classified
            .get(variant)
            .and_then(|(_, answer)| answer.clone())
            .unwrap_or_default();
        let expected = &table_answers[variant];
        if &recorded != expected {
            failures.push(format!(
                "A5 actorless answer {variant}: \"{recorded}\" != table \"{expected}\". The arm \
                 names a different `NoActor` answer than the one adjudicated here — note that \
                 `A3b` accepts any `NoActor::`-qualified path, including an associated const, \
                 so this is the assertion that reads the answer."
            ));
        }
    }

    let simultaneous_class = class.get("Simultaneous").unwrap_or(&empty);
    let simultaneous_table: BTreeSet<String> =
        SIMULTANEOUS.iter().map(|s| (*s).to_string()).collect();
    if simultaneous_class != &simultaneous_table {
        failures.push(format!(
            "A6 simultaneous class {} != {}. CR 103.5 lets exactly the mulligan family decide \
             at once; `Simultaneous` is not an escape hatch for a state that in fact has \
             nobody to act.",
            render_set(simultaneous_class),
            render_set(&simultaneous_table),
        ));
    }

    let no_actor_declared: BTreeSet<String> = no_actor_variants.iter().cloned().collect();
    let no_actor_used: BTreeSet<String> = table_answers.values().cloned().collect();
    if no_actor_declared != no_actor_used {
        failures.push(format!(
            "A7 `NoActor` variants {} != the answers named in `ACTORLESS` {}. A `NoActor` \
             variant with no member is dead vocabulary; a member naming an undeclared answer \
             cannot compile.",
            render_set(&no_actor_declared),
            render_set(&no_actor_used),
        ));
    }

    let source_keys: Vec<&str> = SOURCES.iter().map(|(key, _)| *key).collect();
    for &(variant, answer, (file, consumer), _) in ACTORLESS {
        if file.is_empty() && consumer.is_empty() {
            continue;
        }
        let Some((_, items)) = source_items.iter().find(|(key, _)| *key == file) else {
            failures.push(format!(
                "A9 cannot search `{file}`: this census can search these consumer sources: {}. \
                 Add it to `SOURCES` (and an `include_str!` if the file is not already held).",
                render_strs(&source_keys)
            ));
            continue;
        };
        if !has_free_fn(items, consumer) {
            failures.push(format!(
                "A9 `{consumer}` is not a free `fn` in `{file}`, but `NoActor::{answer}` \
                 (adjudicated for `WaitingFor::{variant}`) still names it. Renaming the consumer \
                 without renaming the answer turns the correspondence into a lie."
            ));
        }
    }

    // A11 — the adapters are adapters. Every other body-reading assertion here reads
    // `acting_authority`'s arms, so this is the only one that can see an adapter
    // answering from a match of its own.
    for adapter in ["acting_player", "acting_players"] {
        let found = inherent_fns(&game_state_items, "WaitingFor", adapter);
        let [f] = found.as_slice() else {
            failures.push(format!(
                "A11 expected exactly one inherent `WaitingFor::{adapter}`, found {}. Zero means \
                 it was renamed, moved or deleted and production no longer reaches \
                 `acting_authority` under that name; two means a second inherent impl block \
                 whose self-type's last path segment is also `WaitingFor` — a `cfg`-gated \
                 duplicate, or a same-named type in an inline `mod` — and this census may be \
                 reading the wrong body. A trait impl is skipped and cannot be the cause.",
                found.len()
            ));
            continue;
        };
        if let Err(why) = adapter_delegation(f) {
            failures.push(format!(
                "A11 `WaitingFor::{adapter}` does not delegate to `acting_authority`: {why}. Its \
                 body must be one tail `match self.acting_authority()`. An adapter that answers \
                 from its own match over `WaitingFor` is read by NO assertion in this file, and \
                 every adjudication here then describes a function production has stopped \
                 asking. A condition that narrows one authority class belongs inside an arm of \
                 the delegating match; one that changes WHO acts belongs in a new `WaitingFor` \
                 variant, exactly as `A8` says of a guard."
            ));
        }
    }

    if declared.len() != 132 {
        failures.push(format!(
            "PIN declared.len()={} != 132.\n\
             \n\
             Adding a `WaitingFor` variant IS the counted event this gate exists to make loud. \
             Repair it by ADJUDICATING, not by bumping the number:\n\
             \x20 * if your new variant has an acting player, its arm answers \
             `ActingAuthority::One` (or `Simultaneous`) and you move this number.\n\
             \x20 * if your new variant has NO acting player, it MUST also appear in \
             `ACTORLESS`, with the `NoActor` answer its arm names and prose saying what \
             advances it.\n\
             \x20 * if `ACTORLESS` looks unchanged after you declared an actorless answer, your \
             declaration is NOT being read — check for a match guard (`A8`) and for a wildcard \
             fallback (`A1`).",
            declared.len()
        ));
    }

    let one_class = class.get("One").unwrap_or(&empty);
    if one_class.len() < 100 {
        failures.push(format!(
            "reach-guard: class[\"One\"].len()={} < 100 — a census that recognised only the \
             actorless arms and dropped the rest could satisfy A4a and A5 while looking healthy",
            one_class.len()
        ));
    }
    if actorless_class.is_empty() {
        failures.push(
            "reach-guard: the actorless class is empty, so A5 is comparing two empty sets"
                .to_string(),
        );
    }

    // ---- Phase 3 — the single terminal assertion. ----
    assert!(
        failures.is_empty(),
        "`WaitingFor::acting_authority` census failed with {} finding(s):\n{}\n\n\
         Read the module doc above before repairing any of these.",
        failures.len(),
        failures.join("\n"),
    );
}

/// CR 103.5: the simultaneous mulligan family has a single *acting player* only while
/// exactly one player is still pending — the historical `pending.len() == 1` rule, which
/// the retyping moved out of the match arms and into `acting_player`.
///
/// A runtime companion to the source census above: the census reads the arms, this drives
/// the adapters. Within this test the len-1 case is what covers `acting_player`'s `[only]`
/// slice arm, and the len-0 and len-3 cases are what cover the `_ => None` beside it, so
/// dropping a case leaves an arm this test no longer exercises. That is a non-vacuity
/// note about this fixture set, not a claim that nothing else in the suite reaches
/// either arm — other tests drive mulligan states, and engine code called during those
/// runs reads `acting_player()` on whatever state is live.
#[test]
fn mulligan_adapters_preserve_the_single_pending_actor_rule() {
    let decision = |n: u8| WaitingFor::MulliganDecision {
        pending: (0..n)
            .map(|i| MulliganDecisionEntry {
                player: PlayerId(i),
                mulligan_count: 0,
                phase: MulliganDecisionPhase::Declare,
            })
            .collect(),
        free_first_mulligan: false,
    };
    let bottoming = |n: u8| WaitingFor::OpeningHandBottomCards {
        pending: (0..n)
            .map(|i| MulliganBottomEntry {
                player: PlayerId(i),
                count: 1,
            })
            .collect(),
        reason: OpeningHandBottomReason::TinyLeadersMultiCommander,
    };

    for state in [decision(0), bottoming(0)] {
        assert_eq!(state.acting_player(), None, "nobody pending, nobody acts");
        assert!(state.acting_players().is_empty());
    }
    for state in [decision(1), bottoming(1)] {
        assert_eq!(
            state.acting_player(),
            Some(PlayerId(0)),
            "exactly one pending player IS the acting player"
        );
        assert_eq!(state.acting_players(), vec![PlayerId(0)]);
    }
    for state in [decision(3), bottoming(3)] {
        assert_eq!(
            state.acting_player(),
            None,
            "CR 103.5: several players decide at once, so there is no single actor"
        );
        assert_eq!(
            state.acting_players(),
            vec![PlayerId(0), PlayerId(1), PlayerId(2)],
            "every pending player may submit, in any arrival order"
        );
    }
}
