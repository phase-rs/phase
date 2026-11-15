//! Tests relocated from `src/types/statics.rs`'s test module.

/// Flatten `Pat::Or` recursively and unwrap `Pat::Paren`, yielding the
/// TOP-LEVEL alternatives of one arm pattern.
fn arm_alternatives(pat: &syn::Pat, out: &mut Vec<syn::Pat>) {
    match pat {
        syn::Pat::Or(or) => or.cases.iter().for_each(|c| arm_alternatives(c, out)),
        syn::Pat::Paren(p) => arm_alternatives(&p.pat, out),
        other => out.push(other.clone()),
    }
}

/// Does this top-level alternative NAME a `StaticMode` variant explicitly?
/// A WHITELIST: it rejects `Pat::Wild` (`_`) and bare-binding `Pat::Ident`
/// (`other`) together, without enumerating irrefutable shapes, so it cannot
/// be defeated by a future irrefutable spelling nobody listed. Sub-patterns
/// INSIDE a `Pat::TupleStruct` / `Pat::Struct` are deliberately out of scope
/// — that is what lets `StaticMode::Other(_)` and
/// `StaticMode::Foo { .. }` through, which are correct Rust and correct
/// design.
fn names_a_static_mode_variant(alt: &syn::Pat) -> bool {
    let path = match alt {
        syn::Pat::Path(p) => &p.path,
        syn::Pat::TupleStruct(p) => &p.path,
        syn::Pat::Struct(p) => &p.path,
        _ => return false,
    };
    path.segments
        .first()
        .is_some_and(|s| s.ident == "StaticMode")
}

/// Locate the single top-level `ExprMatch` in an `ImplItemFn` body.
fn sole_top_level_match(f: &syn::ImplItemFn) -> &syn::ExprMatch {
    let matches: Vec<&syn::ExprMatch> = f
        .block
        .stmts
        .iter()
        .filter_map(|stmt| match stmt {
            syn::Stmt::Expr(syn::Expr::Match(m), _) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "`defending_player_anchor_polarity`'s body must contain EXACTLY ONE \
             top-level `match` expression, or this row silently reads the wrong \
             one (or none); found {}",
        matches.len()
    );
    matches[0]
}

/// CR 508.1c + CR 702.3b, STRUCTURAL half: every arm alternative of
/// `StaticMode::defending_player_anchor_polarity` NAMES a `StaticMode`
/// variant explicitly, so a future `StaticMode` variant is a COMPILE ERROR
/// that must be classified deliberately rather than silently falling into
/// the `None` default.
///
/// A behavioural row (row B) passes identically under `_ => None`; only a
/// source-level assertion can buy this. The predicate is a WHITELIST and is
/// measured green on `StaticMode::as_keyword`, the in-tree spelling
/// precedent this classification is copied from.
#[test]
fn defending_player_anchor_polarity_names_every_arm() {
    // ROUTED through the repo's single comment-policy authority
    // (`crate::source_census`), per `no_source_reading_file_carries_a_private_comment_policy`:
    // every line's comment half is removed by the SHARED rule before the
    // source is read, so a doc comment quoting an irrefutable arm cannot be
    // read as one.
    let stripped =
        crate::source_census::code_lines(include_str!("../../../phase-ast/src/types/statics.rs"));
    let file = syn::parse_file(&stripped)
        .expect("this file must parse as Rust for the structural row to mean anything");

    let mut found: Option<syn::ImplItemFn> = None;
    for item in &file.items {
        let syn::Item::Impl(imp) = item else { continue };
        for sub in &imp.items {
            let syn::ImplItem::Fn(f) = sub else { continue };
            if f.sig.ident == "defending_player_anchor_polarity" {
                assert!(
                    found.is_none(),
                    "exactly one `defending_player_anchor_polarity` must exist"
                );
                found = Some(f.clone());
            }
        }
    }
    let f = found.expect(
        "`StaticMode::defending_player_anchor_polarity` must exist — this row is \
             reading the real function, not a renamed or deleted one",
    );

    // (a) exactly one top-level `ExprMatch`.
    let m = sole_top_level_match(&f);

    // PAIRED POSITIVE CONTROL: the row is reading the real classification.
    assert!(
        m.arms.len() >= 3,
        "the classification must have at least three arms; got {}",
        m.arms.len()
    );
    let rendered: Vec<String> = m.arms.iter().map(|a| quote_pat(&a.pat)).collect();
    for expected in [
        "StaticMode :: CanAttackWithDefender",
        "StaticMode :: CantAttack",
        "StaticMode :: CantAttackOrBlock",
    ] {
        assert!(
            rendered.iter().any(|r| r.contains(expected)),
            "the classification must still mention {expected}; arms were {rendered:?}"
        );
    }

    // (b) every top-level alternative of every arm names a variant.
    let mut total_alternatives = 0usize;
    for arm in &m.arms {
        let mut alts = Vec::new();
        arm_alternatives(&arm.pat, &mut alts);
        for alt in &alts {
            total_alternatives += 1;
            assert!(
                names_a_static_mode_variant(alt),
                "EVERY arm alternative must NAME a `StaticMode` variant \
                     explicitly — no `_`, no bare-binding catch-all. Offending \
                     alternative: {}",
                quote_pat(alt)
            );
        }
    }
    assert!(
        total_alternatives >= 3,
        "instrument control: the walk must have seen alternatives at all; got \
             {total_alternatives}"
    );

    // (c) THE INSTRUMENT FIRES. Without this a predicate that accepted
    // everything would satisfy (b) vacuously.
    for irrefutable in [
        "_ => None",
        "other => None",
        "StaticMode::CantAttack | _ => None",
    ] {
        let arm: syn::Arm = syn::parse_str(irrefutable)
            .unwrap_or_else(|e| panic!("{irrefutable} must parse as an arm: {e}"));
        let mut alts = Vec::new();
        arm_alternatives(&arm.pat, &mut alts);
        assert!(
            !alts.iter().all(names_a_static_mode_variant),
            "the predicate must REJECT the irrefutable arm `{irrefutable}`"
        );
    }
    for accepted in [
        "StaticMode::Other(_) => None",
        "StaticMode::Continuous => None",
        "StaticMode::IgnoreLandwalkForBlocking { .. } => None",
        "StaticMode::CantAttack | StaticMode::CantBlock => None",
    ] {
        let arm: syn::Arm = syn::parse_str(accepted)
            .unwrap_or_else(|e| panic!("{accepted} must parse as an arm: {e}"));
        let mut alts = Vec::new();
        arm_alternatives(&arm.pat, &mut alts);
        assert!(
            alts.iter().all(names_a_static_mode_variant),
            "the predicate must ACCEPT the variant-naming arm `{accepted}` — \
                 sub-patterns inside a variant are deliberately out of scope"
        );
    }
}

/// Render a `syn::Pat` for assertion messages without pulling in `quote`.
fn quote_pat(pat: &syn::Pat) -> String {
    use syn::__private::ToTokens;
    pat.to_token_stream().to_string()
}
