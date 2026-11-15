//! Layer-boundary census for the crate split: the field-type clauses 4b and 4d.
//!
//! The AST and parser layers are their own crates, `phase-ast` and `phase-parser`, so the
//! compiler keeps them from naming the game side, and the census keeps only the field-type
//! clauses 4b and 4d. It parses the source with `syn` instead of grepping it, so comments and doc
//! text cannot count as references.
//!
//! What it walks:
//!
//! * **Clause 4b.** Every `struct` field, enum-variant field and `type` alias under
//!   `crates/engine/src`, `crates/phase-ast/src` and `crates/phase-parser/src`, test items and
//!   inline `mod` blocks included: `EpicEffectId` is a field type only of `Effect::EpicCopy`.
//!   The decode check that refuses a dangling Epic handle recognizes that one serialized shape,
//!   so a second carrier type would escape it. Item macros (such as `declare_game_state!`, which
//!   declares `GameState`'s fields) are scanned by token: one whose tokens name `EpicEffectId` is
//!   a hit.
//! * **Clause 4d.** As 4b, for `ReplacementContinuationId`: it is a field type only of
//!   `ReplacementDefinition::runtime_execute`, the one serialized shape the decode check
//!   recognizes.

use std::path::{Path, PathBuf};

use syn::__private::ToTokens;
use syn::parse::Parser;

use super::loop_shortcut_offer_writer_census::rs_files;

/// Flatten a token stream to idents, punctuation (`::` joined), `<lit>…` literals and
/// `<g` / `g>` group markers.
fn flatten(ts: impl ToTokens) -> Vec<String> {
    let parser = |input: syn::parse::ParseStream| {
        input.step(|cursor| {
            let mut out = Vec::new();
            walk_cursor(*cursor, &mut out);
            let mut end = *cursor;
            while let Some((_, next)) = end.token_tree() {
                end = next;
            }
            Ok((out, end))
        })
    };
    parser
        .parse2(ts.to_token_stream())
        .expect("token stream re-parses")
}

fn walk_cursor(mut c: syn::buffer::Cursor, out: &mut Vec<String>) {
    loop {
        if let Some((ident, next)) = c.ident() {
            out.push(ident.to_string());
            c = next;
        } else if let Some((punct, next)) = c.punct() {
            let ch = punct.as_char();
            match out.last_mut() {
                Some(last) if ch == ':' && last == ":" => *last = "::".to_string(),
                _ => out.push(ch.to_string()),
            }
            c = next;
        } else if let Some((inner, _, _, next)) = c.any_group() {
            out.push("<g".to_string());
            walk_cursor(inner, out);
            out.push("g>".to_string());
            c = next;
        } else if let Some((lit, next)) = c.literal() {
            out.push(format!("<lit>{lit}"));
            c = next;
        } else if let Some((_, next)) = c.lifetime() {
            out.push("'_".to_string());
            c = next;
        } else {
            break;
        }
    }
}

fn engine_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// One declared field whose type names a marker: `(file, item, variant, field)`. A tuple
/// field is named by its index, a `type` alias by `type`, and an item macro by `macro` (its
/// item is the macro path).
type FieldHit = (String, String, Option<String>, String);

/// Clauses 4b and 4d: every struct field, enum-variant field (named and tuple) and `type` alias in
/// `src`, inline `mod` blocks included, whose flattened type names `marker`, and every item
/// macro whose tokens name it. Function bodies are not declarations, so a local of that type
/// is not a hit.
fn fields_naming(file: &str, src: &str, marker: &str) -> Vec<FieldHit> {
    let parsed = syn::parse_file(src).unwrap_or_else(|e| panic!("parse {file}: {e}"));
    items_fields_naming(file, &parsed.items, marker)
}

/// [`fields_naming`] over already-parsed items.
fn items_fields_naming(file: &str, items: &[syn::Item], marker: &str) -> Vec<FieldHit> {
    fn names(ty: &syn::Type, marker: &str) -> bool {
        flatten(ty).iter().any(|token| token == marker)
    }
    fn field_hits<'a>(
        fields: impl IntoIterator<Item = &'a syn::Field>,
        marker: &str,
    ) -> Vec<String> {
        fields
            .into_iter()
            .enumerate()
            .filter(|(_, field)| names(&field.ty, marker))
            .map(|(index, field)| {
                field
                    .ident
                    .as_ref()
                    .map_or_else(|| index.to_string(), ToString::to_string)
            })
            .collect()
    }
    fn collect(file: &str, items: &[syn::Item], marker: &str, out: &mut Vec<FieldHit>) {
        for item in items {
            match item {
                syn::Item::Struct(s) => out.extend(
                    field_hits(&s.fields, marker)
                        .into_iter()
                        .map(|field| (file.to_string(), s.ident.to_string(), None, field)),
                ),
                syn::Item::Enum(e) => {
                    for variant in &e.variants {
                        out.extend(
                            field_hits(&variant.fields, marker)
                                .into_iter()
                                .map(|field| {
                                    (
                                        file.to_string(),
                                        e.ident.to_string(),
                                        Some(variant.ident.to_string()),
                                        field,
                                    )
                                }),
                        );
                    }
                }
                syn::Item::Type(t) if names(&t.ty, marker) => out.push((
                    file.to_string(),
                    t.ident.to_string(),
                    None,
                    "type".to_string(),
                )),
                syn::Item::Mod(m) => {
                    if let Some((_, inner)) = &m.content {
                        collect(file, inner, marker, out);
                    }
                }
                syn::Item::Macro(m)
                    if flatten(&m.mac.tokens).iter().any(|token| token == marker) =>
                {
                    out.push((
                        file.to_string(),
                        flatten(&m.mac.path).concat(),
                        None,
                        "macro".to_string(),
                    ));
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    collect(file, items, marker, &mut out);
    out
}

/// Clauses 4b and 4d: every field under `crates/engine/src`, `crates/phase-ast/src` and
/// `crates/phase-parser/src` whose type names `marker` is exactly `expected`,
/// `(file, item, variant, field)`, each file relative to its crate's `src/`.
fn assert_handle_held_only_by(
    marker: &str,
    expected: &[(&str, &str, Option<&str>, &str)],
    reason: &str,
) {
    let engine = engine_src();
    let ast = Path::new(env!("CARGO_MANIFEST_DIR")).join("../phase-ast/src");
    let parser = Path::new(env!("CARGO_MANIFEST_DIR")).join("../phase-parser/src");
    let mut hits = Vec::new();
    let mut walked = Vec::new();
    for src in [&engine, &ast, &parser] {
        for path in rs_files(src) {
            let text =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
            walked.push(path.clone());
            // A sound prefilter: a declaration can name the type only if its file text does.
            if !text.contains(marker) {
                continue;
            }
            let rel = path
                .strip_prefix(src)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            hits.extend(fields_naming(&rel, &text, marker));
        }
    }
    assert!(
        walked
            .iter()
            .any(|path| path.ends_with("phase-parser/src/parser/oracle.rs")),
        "4b/4d census no longer walks the phase-parser crate"
    );
    hits.sort();
    let expected: Vec<FieldHit> = expected
        .iter()
        .map(|(file, item, variant, field)| {
            (
                file.to_string(),
                item.to_string(),
                variant.map(str::to_string),
                field.to_string(),
            )
        })
        .collect();
    assert_eq!(hits, expected, "{reason}");
}

#[test]
fn epic_effect_handle_is_a_field_only_of_effect_epic_copy() {
    assert_handle_held_only_by(
        "EpicEffectId",
        &[("types/ability.rs", "Effect", Some("EpicCopy"), "epic")],
        "`EpicEffectId` must be a field type only of `Effect::EpicCopy`: the decode check \
         (`collect_serialized_state_handles`) recognizes only that serialized shape, so \
         a handle held anywhere else would escape the dangling-handle refusal",
    );
}

#[test]
fn replacement_continuation_handle_is_a_field_only_of_replacement_definition() {
    assert_handle_held_only_by(
        "ReplacementContinuationId",
        &[(
            "types/ability.rs",
            "ReplacementDefinition",
            None,
            "runtime_execute",
        )],
        "`ReplacementContinuationId` must be a field type only of \
         `ReplacementDefinition::runtime_execute`: the decode check \
         (`collect_serialized_state_handles`) recognizes only the `runtime_execute` key, so a \
         handle held anywhere else would escape the dangling-handle refusal",
    );
}

#[test]
fn the_field_census_flags_planted_state_handles() {
    for marker in ["EpicEffectId", "ReplacementContinuationId"] {
        let one_hit = |label: &str, src: &str| {
            let src = src.replace("MARKER", marker);
            let hits = fields_naming("<planted>", &src, marker);
            assert_eq!(
                hits.len(),
                1,
                "{marker} {label}: expected exactly one hit, got {hits:#?}"
            );
        };
        one_hit("named struct field", "struct S { a: Option<MARKER> }");
        one_hit("tuple variant field", "enum E { V(Vec<MARKER>) }");
        one_hit("type alias", "type A = MARKER;");
        one_hit(
            "inline module",
            "mod m { pub struct S { a: crate::types::identifiers::MARKER } }",
        );
        one_hit(
            "item macro",
            "declare_game_state! { pub x: Option<MARKER>, }",
        );
        let local = fields_naming(
            "<planted>",
            &"fn f() { let x: MARKER = MARKER(0); }".replace("MARKER", marker),
            marker,
        );
        assert!(
            local.is_empty(),
            "{marker}: a local is not a field: {local:#?}"
        );
    }
}
