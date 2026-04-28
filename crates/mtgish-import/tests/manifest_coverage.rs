//! Source-parse engine type files and assert every `Vec<T>` /
//! `im::Vector<T>` field is classified by `ORDERING_MANIFEST`.
//!
//! The diff infrastructure relies on the manifest being exhaustive: any
//! unclassified list field falls back to `OrderSignificant` (the safe
//! default), but a silent fallback hides design intent. This test fails
//! whenever a new list field is added without an accompanying manifest
//! entry.
//!
//! Strategy:
//! 1. Read each engine type file via `syn`.
//! 2. Walk every `struct` and named-field enum variant.
//! 3. For each field whose type is `Vec<...>` or `im::Vector<...>`,
//!    record `(carrier_name, field_name)`.
//! 4. Subtract the manifest entries; the test prints the missing pairs
//!    so the developer can extend `ORDERING_MANIFEST`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use mtgish_import::diff::ORDERING_MANIFEST;

const ENGINE_TYPE_FILES: &[&str] = &[
    "crates/engine/src/types/ability.rs",
    "crates/engine/src/types/triggers.rs",
    "crates/engine/src/types/replacements.rs",
    "crates/engine/src/types/statics.rs",
    "crates/engine/src/types/card.rs",
];

#[test]
fn every_list_field_is_in_ordering_manifest() {
    let workspace_root = workspace_root();
    let mut all_fields: BTreeSet<(String, String)> = BTreeSet::new();
    for rel in ENGINE_TYPE_FILES {
        let path = workspace_root.join(rel);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let file =
            syn::parse_file(&src).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        collect_list_fields(&file, &mut all_fields);
    }

    let manifest_entries: BTreeSet<(String, String)> = ORDERING_MANIFEST
        .iter()
        .map(|((c, f), _)| ((*c).to_string(), (*f).to_string()))
        .collect();

    let missing: Vec<&(String, String)> = all_fields
        .iter()
        .filter(|pair| !manifest_entries.contains(pair))
        .collect();

    if !missing.is_empty() {
        let mut msg = String::from(
            "ORDERING_MANIFEST is missing entries for the following Vec<...> fields.\n\
             Add a `((carrier, field), OrderingClass::*)` entry in src/diff/ordering.rs.\n\n",
        );
        for (c, f) in &missing {
            msg.push_str(&format!("  ({c}, {f})\n"));
        }
        panic!("{msg}");
    }
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR for an integration test is the crate root.
    // Walk up two levels (../../) to reach the workspace root.
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or(crate_dir)
}

fn collect_list_fields(file: &syn::File, out: &mut BTreeSet<(String, String)>) {
    for item in &file.items {
        match item {
            syn::Item::Struct(s) => {
                let carrier = s.ident.to_string();
                if let syn::Fields::Named(named) = &s.fields {
                    for field in &named.named {
                        if let Some(name) = field.ident.as_ref() {
                            if is_list_type(&field.ty) {
                                out.insert((carrier.clone(), name.to_string()));
                            }
                        }
                    }
                }
            }
            syn::Item::Enum(e) => {
                let carrier = e.ident.to_string();
                for variant in &e.variants {
                    if let syn::Fields::Named(named) = &variant.fields {
                        for field in &named.named {
                            if let Some(name) = field.ident.as_ref() {
                                if is_list_type(&field.ty) {
                                    out.insert((carrier.clone(), name.to_string()));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Whether the given type is a list-shaped container we expect the
/// manifest to classify: `Vec<T>`, `Option<Vec<T>>`, or `im::Vector<T>`.
///
/// `Option<Vec<T>>` is included because it produces the same JSON shape
/// at the diff layer (an optional array). Boxed wrappers are unwrapped.
fn is_list_type(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Path(tp) => {
            let last = match tp.path.segments.last() {
                Some(s) => s,
                None => return false,
            };
            let name = last.ident.to_string();
            match name.as_str() {
                "Vec" | "Vector" => true,
                "Option" | "Box" => {
                    if let syn::PathArguments::AngleBracketed(args) = &last.arguments {
                        args.args.iter().any(|a| match a {
                            syn::GenericArgument::Type(inner) => is_list_type(inner),
                            _ => false,
                        })
                    } else {
                        false
                    }
                }
                _ => false,
            }
        }
        _ => false,
    }
}
