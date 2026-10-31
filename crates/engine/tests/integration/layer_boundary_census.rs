//! Layer-boundary census for the crate split: parser library code names no game-side module
//! and no state-layer item, and parser test code crosses no future crate boundary.
//!
//! The AST layer is its own crate, `phase-ast`, so the compiler keeps it from naming the parser
//! or the game side. Until the parser is its own crate too, nothing but this census stops a
//! parser edit from reaching back into `game` (or any other game-side module) or into the state
//! layer, and silently re-coupling the layers. The census parses the source with `syn` instead
//! of grepping it, so comments and doc text cannot count as references.
//!
//! What it walks:
//!
//! * **Clause 1.** The module tree from `src/parser/mod.rs`, following `mod x;`, `#[path]`
//!   and inline `mod` blocks. Every file is recorded as library code or test-only code; an
//!   item is test-only when its `cfg` predicate implies `test`. Library `use` trees are
//!   resolved against the current module path (`crate`, `self`, `super` chains, groups,
//!   renames, globs). Every other library item is flattened to tokens: a path whose head
//!   resolves to the crate root followed by a game-side segment, a bare game-side head
//!   followed by `::`, an exported game-side macro followed by `!`, or a string literal
//!   naming a `crate::<game-side>::` path is a hit.
//! * **Clause 2.** `lib.rs` (every `use`) and the `types/` tree (every non-private `use`):
//!   neither the crate root nor `types` may re-export a game-side module, or a parser path
//!   through them would cross the boundary without naming it.
//!
//! * **Clause 4b.** Every `struct` field, enum-variant field and `type` alias under
//!   `crates/engine/src` and `crates/phase-ast/src`, test items and inline `mod` blocks
//!   included: `EpicEffectId` is a field type only of `Effect::EpicCopy`. The decode check that
//!   refuses a dangling Epic handle recognizes that one serialized shape, so a second carrier
//!   type would escape it. Item macros (such as `declare_game_state!`, which declares
//!   `GameState`'s fields) are scanned by token: one whose tokens name `EpicEffectId` is a hit.
//! * **Clause 4d.** As 4b, for `ReplacementContinuationId`: it is a field type only of
//!   `ReplacementDefinition::runtime_execute`, the one serialized shape the decode check
//!   recognizes.
//! * **Clause 5.** One index of the whole crate from `lib.rs` (test-only modules skipped):
//!   per module its definitions, `use` entries, child modules and impl blocks, with a resolver
//!   for `crate` / `self` / `super` chains, imported names, child modules, globs and
//!   longest-prefix item resolution. A `#[macro_export]` macro is also an item of the crate
//!   root, and a bare name nothing binds is taken as a crate-root macro of that name when one
//!   exists (an over-approximation: Rust reaches it bare only through an import). A
//!   `macro_rules!` in `types/` whose body declares `struct <Ident>` defines that struct at
//!   each invocation (`GameState`). A unit is a
//!   `types/` item's definition or body, a member of a `types/`-located impl on it, or a
//!   `types/` macro (`macro_rules!` or item-level invocation); its references are its resolved
//!   paths and its `serde` attribute path strings. The state layer T is the fixpoint of
//!   `types/` items whose definitions name a game-side item, module or T item, seeded by impl
//!   members that name a game-side item and extended by member signatures that name a T item.
//!   A façade is a non-root `types/` file whose library `use` entries hold the glob
//!   `phase_ast::types::<its module>::*`: the engine half of a file whose AST half is in
//!   `phase-ast`. The state modules are the other non-root `types/` files.
//!   * **5a.** Parser library code names no T item (directly or through a `types` root
//!     re-export), and no parser path or `use` passes through a state module.
//!   * The AST closure over this index (from the `types/` items parser library code names and
//!     the macro units of the files that are no state module, stopping at T) holds no item
//!     outside T: such an item is an AST item left in `phase-engine`, reported with the chain
//!     that reached it.
//!
//! Fail-closed limits: a `#[cfg(test)]` on a statement inside a library function is not
//! honoured (the statement is scanned as library code), and a `#[path]` inside an inline
//! module panics instead of being guessed at. Clause 5's fail-open limits: a re-export chain
//! deeper than `RESOLVE_DEPTH` resolves to nothing; a struct a macro declares through a
//! metavariable name is not indexed; a statement-level `use` inside a function binds
//! nothing: its own paths are checked, but a path through a module it imports
//! (`use crate::types::game_state as gs; gs::GameState`, or one through `{self}`) is not
//! resolved, and in parser code such a `use` of a state module is not itself reported, as a
//! module-level one is; a trait that a glob or a function-local glob brings into scope, called
//! by method syntax, is not seen: method resolution needs types; and the index reads one crate:
//! a path that reaches a name the index does not hold ends at the module it entered, naming no
//! item, and no path continues through a name such a path binds.
//!
//! * **Clause 6.** The test regions of `parser/` files cross no future crate boundary. An
//!   `Overlay::Tests` build of clause 5's index also reads the test-only items and `mod` files
//!   (`#[path]` included) under `parser/`, and each library `#[test]` fn, into separate overlay
//!   maps; `Scope::Tests` resolution reads the overlay first and then the library maps, and
//!   clause 5 reads only the library maps, so its sets are unchanged. A test unit is judged on
//!   its own references and a `#[test]` on its closure over the overlay helpers it names. The
//!   classes: a game-side item, module or passage; the crate-level `test_support` or
//!   `source_census`; a T item or state-module passage; and a source read (`include_str!` /
//!   `include_bytes!` resolved against the file's directory, or `env!("CARGO_MANIFEST_DIR")`
//!   with the literal that follows it) of a `crates/engine/` path outside `src/parser/`, a
//!   `CARGO_MANIFEST_DIR` with no adjacent literal failing closed. Every overlay `use` entry
//!   naming such a target, or globbing such a module, is a `<use>` hit. Clause 6's fail-open
//!   limits: `std::fs` reads with cwd-relative paths, and paths built at runtime rather than
//!   by a literal, are not seen; a statement-level `use` in a test fn binds nothing, as in
//!   clause 5; clause 5's resolver limits apply (a re-export chain deeper than
//!   `RESOLVE_DEPTH`, a struct declared through a macro metavariable, and a trait a
//!   function-local glob brings into scope called by method syntax); and an identifier reached
//!   only through a format-string capture is no reference, so it adds no helper edge and names
//!   no item.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use syn::__private::ToTokens;
use syn::parse::Parser;

use super::loop_shortcut_offer_writer_census::rs_files;

/// The game-side modules of `crates/engine/src`: everything above the parser and AST layers.
const GAME_SIDE: &[&str] = &[
    "ai_support",
    "analysis",
    "database",
    "game",
    "starter_decks",
    "testing",
    "util",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Hit {
    file: String,
    module: String,
    shape: String,
}

/// Which items of a walked file are scanned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// Clause 1: every library item.
    Library,
    /// Clause 2: only `use` items with the given visibility.
    Reexports(UseVisibility),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UseVisibility {
    Any,
    NonPrivate,
}

/// Whether a walked file's `mod x;` declarations are followed into their files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Children {
    Follow,
    Skip,
}

/// Whether a walked file is reached as library code or only through test-only modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    Library,
    TestOnly,
}

struct Walk {
    src: PathBuf,
    scan: Scan,
    /// The top-level modules a hit names: `GAME_SIDE`.
    targets: &'static [&'static str],
    macros: Vec<String>,
    hits: Vec<Hit>,
    files: Vec<(PathBuf, Reach)>,
}

impl Walk {
    fn new(src: &Path, scan: Scan, targets: &'static [&'static str], macros: Vec<String>) -> Self {
        Self {
            src: src.to_path_buf(),
            scan,
            targets,
            macros,
            hits: Vec::new(),
            files: Vec::new(),
        }
    }

    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.src)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn hit(&mut self, file: &str, module: &[String], shape: String) {
        self.hits.push(Hit {
            file: file.to_string(),
            module: module.join("::"),
            shape,
        });
    }

    fn files_reached(&self, reach: Reach) -> BTreeSet<String> {
        self.files
            .iter()
            .filter(|(_, r)| *r == reach)
            .map(|(p, _)| self.rel(p))
            .collect()
    }

    fn walk_file(&mut self, file: &Path, module: Vec<String>, reach: Reach, children: Children) {
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        let parsed = syn::parse_file(&text).unwrap_or_else(|e| panic!("parse {file:?}: {e}"));
        self.files.push((file.to_path_buf(), reach));
        let name = file.file_name().expect("file name").to_string_lossy();
        let dir = file.parent().expect("file has a parent").to_path_buf();
        let child_dir = if name == "mod.rs" || name == "lib.rs" {
            dir.clone()
        } else {
            dir.join(file.file_stem().expect("file stem"))
        };
        let rel = self.rel(file);
        let ctx = FileCtx {
            rel: &rel,
            dir: &dir,
            children,
        };
        self.walk_items(&parsed.items, &ctx, &child_dir, false, &module, reach);
    }

    fn walk_items(
        &mut self,
        items: &[syn::Item],
        ctx: &FileCtx<'_>,
        child_dir: &Path,
        inline: bool,
        module: &[String],
        reach: Reach,
    ) {
        for item in items {
            let item_reach = if reach == Reach::TestOnly || is_test_only(item_attrs(item)) {
                Reach::TestOnly
            } else {
                Reach::Library
            };
            if let syn::Item::Mod(m) = item {
                let mut path = module.to_vec();
                path.push(m.ident.to_string());
                match &m.content {
                    Some((_, inner)) => {
                        let dir = child_dir.join(m.ident.to_string());
                        self.walk_items(inner, ctx, &dir, true, &path, item_reach);
                    }
                    None if ctx.children == Children::Follow => {
                        let candidates =
                            mod_file_candidates(m, ctx.rel, ctx.dir, child_dir, inline);
                        let target = candidates
                            .iter()
                            .find(|p| p.exists())
                            .or(candidates.last())
                            .expect("a `mod` declaration has a candidate file")
                            .clone();
                        self.walk_file(&target, path, item_reach, Children::Follow);
                    }
                    None => {}
                }
                continue;
            }
            if item_reach == Reach::TestOnly {
                continue;
            }
            self.scan_item(item, ctx.rel, module);
        }
    }

    fn scan_item(&mut self, item: &syn::Item, file: &str, module: &[String]) {
        if let syn::Item::Use(u) = item {
            let counts = match self.scan {
                Scan::Library | Scan::Reexports(UseVisibility::Any) => true,
                Scan::Reexports(UseVisibility::NonPrivate) => {
                    !matches!(u.vis, syn::Visibility::Inherited)
                }
            };
            if counts {
                self.scan_use(&u.tree, Vec::new(), module, file);
            }
            return;
        }
        if self.scan != Scan::Library {
            return;
        }
        let toks = match item {
            syn::Item::Impl(imp) => {
                let mut imp = imp.clone();
                imp.items.retain(|ii| !is_test_only(impl_item_attrs(ii)));
                flatten(&imp)
            }
            syn::Item::Trait(tr) => {
                let mut tr = tr.clone();
                tr.items.retain(|ti| !is_test_only(trait_item_attrs(ti)));
                flatten(&tr)
            }
            other => flatten(other),
        };
        self.scan_tokens(&toks, module, file);
    }

    fn scan_use(
        &mut self,
        tree: &syn::UseTree,
        prefix: Vec<String>,
        module: &[String],
        file: &str,
    ) {
        match tree {
            syn::UseTree::Path(p) => {
                let next = use_step(&prefix, &p.ident.to_string(), module);
                self.scan_use(&p.tree, next, module, file);
            }
            syn::UseTree::Name(n) => {
                let path = use_step(&prefix, &n.ident.to_string(), module);
                self.check_use(&path, "name", module, file);
            }
            syn::UseTree::Rename(r) => {
                let path = use_step(&prefix, &r.ident.to_string(), module);
                self.check_use(&path, "rename", module, file);
            }
            syn::UseTree::Glob(_) => {
                if prefix == ["crate"] {
                    self.hit(file, module, "use glob of crate root".into());
                } else {
                    self.check_use(&prefix, "glob", module, file);
                }
            }
            syn::UseTree::Group(g) => {
                for t in &g.items {
                    self.scan_use(t, prefix.clone(), module, file);
                }
            }
        }
    }

    fn check_use(&mut self, path: &[String], shape: &str, module: &[String], file: &str) {
        let head_is_root_or_bare = path.first().is_some_and(|f| f == "crate" || f == "?");
        if head_is_root_or_bare
            && path
                .get(1)
                .is_some_and(|s| self.targets.contains(&s.as_str()))
        {
            self.hit(file, module, format!("use {shape} {}", path.join("::")));
        }
    }

    fn scan_tokens(&mut self, toks: &[String], module: &[String], file: &str) {
        for i in 0..toks.len() {
            let t = toks[i].as_str();
            let prev_is_sep = i > 0 && toks[i - 1] == "::";
            if !prev_is_sep {
                match root_resolution(toks, i, module) {
                    // The head resolves to the crate root: `after` indexes the `::` that follows it.
                    Some((0, after)) => {
                        for segment in root_segments(toks, after, self.targets) {
                            if self.targets.contains(&segment) {
                                self.hit(file, module, format!("{t}-headed path to {segment}"));
                            }
                        }
                    }
                    Some(_) => {}
                    None => {
                        if self.targets.contains(&t) && toks.get(i + 1).is_some_and(|n| n == "::") {
                            self.hit(file, module, format!("bare-headed path to {t}"));
                        }
                    }
                }
            }
            if let Some(lit) = t.strip_prefix("<lit>") {
                let names_target = syn::parse_str::<syn::LitStr>(lit).is_ok_and(|s| {
                    let value = s.value();
                    self.targets.iter().any(|g| {
                        value
                            .strip_prefix("crate::")
                            .and_then(|rest| rest.strip_prefix(g))
                            .is_some_and(|rest| rest.starts_with("::"))
                    })
                });
                if names_target {
                    self.hit(file, module, format!("path string {lit}"));
                }
            }
            if self.macros.iter().any(|m| m == t) && toks.get(i + 1).is_some_and(|n| n == "!") {
                self.hit(file, module, format!("exported macro {t}!"));
            }
        }
    }
}

struct FileCtx<'a> {
    rel: &'a str,
    dir: &'a Path,
    children: Children,
}

/// For a `crate` or `super` chain head at `i`, the depth it resolves to below the crate root
/// and the index of the `::` after the chain. `None` for any other token.
fn root_resolution(toks: &[String], i: usize, module: &[String]) -> Option<(usize, usize)> {
    match toks[i].as_str() {
        "crate" => Some((0, i + 1)),
        "super" => {
            let mut depth = module.len();
            let mut j = i;
            loop {
                depth = depth.saturating_sub(1);
                let chained = toks.get(j + 1).is_some_and(|t| t == "::")
                    && toks.get(j + 2).is_some_and(|t| t == "super");
                if !chained {
                    break;
                }
                j += 2;
            }
            Some((depth, j + 1))
        }
        _ => None,
    }
}

/// The first segments reached from a crate-root head: the segment after `::`; for a
/// statement-level `use crate::{…}` group inside a function body, each top-level entry that is
/// a bare name (an entry followed by `::` is already a bare-headed path to the scan); or every
/// target module for a `*` glob of the crate root.
fn root_segments<'a>(toks: &'a [String], after: usize, targets: &[&'a str]) -> Vec<&'a str> {
    if toks.get(after).is_none_or(|t| t != "::") {
        return Vec::new();
    }
    match toks.get(after + 1).map(String::as_str) {
        Some("<g") => {
            let mut out = Vec::new();
            let mut depth = 0usize;
            let mut at_entry = true;
            for (k, t) in toks.iter().enumerate().skip(after + 2) {
                match t.as_str() {
                    "<g" => depth += 1,
                    "g>" if depth == 0 => break,
                    "g>" => depth -= 1,
                    "," if depth == 0 => at_entry = true,
                    entry if at_entry => {
                        if toks.get(k + 1).is_none_or(|n| n != "::") {
                            out.push(entry);
                        }
                        at_entry = false;
                    }
                    _ => {}
                }
            }
            out
        }
        Some("*") => targets.to_vec(),
        Some(segment) => vec![segment],
        None => Vec::new(),
    }
}

/// Resolve one `use`-tree segment against the resolved `prefix`. An unresolvable head
/// (an extern crate or a local name) is recorded as `["?", ident]`.
fn use_step(prefix: &[String], ident: &str, module: &[String]) -> Vec<String> {
    if prefix.is_empty() {
        return match ident {
            "crate" => vec!["crate".into()],
            "self" => std::iter::once("crate".to_string())
                .chain(module.iter().cloned())
                .collect(),
            "super" => {
                let mut v: Vec<String> = std::iter::once("crate".to_string())
                    .chain(module.iter().cloned())
                    .collect();
                if v.len() > 1 {
                    v.pop();
                }
                v
            }
            _ => vec!["?".into(), ident.to_string()],
        };
    }
    let mut p = prefix.to_vec();
    match ident {
        "super" if p.first().is_some_and(|f| f == "crate") => {
            if p.len() > 1 {
                p.pop();
            }
        }
        "self" => {}
        _ => p.push(ident.to_string()),
    }
    p
}

/// A `cfg` predicate implies `test` iff it is `test`, an `all(…)` with any implying child,
/// or a non-empty `any(…)` whose children all imply it. `not(…)` and `feature = …` do not.
fn cfg_implies_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(p) => p.is_ident("test"),
        syn::Meta::List(list) => {
            let Ok(args) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) else {
                return false;
            };
            if list.path.is_ident("all") {
                args.iter().any(cfg_implies_test)
            } else if list.path.is_ident("any") {
                !args.is_empty() && args.iter().all(cfg_implies_test)
            } else {
                false
            }
        }
        syn::Meta::NameValue(_) => false,
    }
}

fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && a.parse_args::<syn::Meta>()
                .is_ok_and(|m| cfg_implies_test(&m))
    })
}

/// The files a `mod x;` declaration may name, in the order they are tried: its `#[path]`
/// target, or `x.rs` and then `x/mod.rs`.
fn mod_file_candidates(
    m: &syn::ItemMod,
    file: &str,
    dir: &Path,
    child_dir: &Path,
    inline: bool,
) -> Vec<PathBuf> {
    match path_attr(&m.attrs) {
        Some(p) => {
            assert!(!inline, "#[path] inside an inline module in {file}");
            vec![dir.join(p)]
        }
        None => vec![
            child_dir.join(format!("{}.rs", m.ident)),
            child_dir.join(m.ident.to_string()).join("mod.rs"),
        ],
    }
}

fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|a| match &a.meta {
        syn::Meta::NameValue(nv) if nv.path.is_ident("path") => match &nv.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Some(s.value()),
            _ => None,
        },
        _ => None,
    })
}

fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::ForeignMod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

fn impl_item_attrs(item: &syn::ImplItem) -> &[syn::Attribute] {
    match item {
        syn::ImplItem::Const(x) => &x.attrs,
        syn::ImplItem::Fn(x) => &x.attrs,
        syn::ImplItem::Type(x) => &x.attrs,
        syn::ImplItem::Macro(x) => &x.attrs,
        _ => &[],
    }
}

fn trait_item_attrs(item: &syn::TraitItem) -> &[syn::Attribute] {
    match item {
        syn::TraitItem::Const(x) => &x.attrs,
        syn::TraitItem::Fn(x) => &x.attrs,
        syn::TraitItem::Type(x) => &x.attrs,
        syn::TraitItem::Macro(x) => &x.attrs,
        _ => &[],
    }
}

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

/// Whether a `macro_rules!` is `#[macro_export]`, and so an item of the crate root.
fn is_macro_export(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("macro_export"))
}

/// `#[macro_export]` macros defined in the `targets` modules.
fn exported_macros(src: &Path, targets: &[&str]) -> Vec<String> {
    fn collect(items: &[syn::Item], out: &mut Vec<String>) {
        for item in items {
            match item {
                syn::Item::Macro(m) if is_macro_export(&m.attrs) => {
                    out.extend(m.ident.as_ref().map(ToString::to_string));
                }
                syn::Item::Mod(m) => {
                    if let Some((_, inner)) = &m.content {
                        collect(inner, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut macros = Vec::new();
    for g in targets {
        let dir = src.join(g);
        let mut files = if dir.is_dir() {
            rs_files(&dir)
        } else {
            Vec::new()
        };
        let flat = src.join(format!("{g}.rs"));
        if flat.exists() {
            files.push(flat);
        }
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap_or_else(|e| panic!("read {f:?}: {e}"));
            let parsed = syn::parse_file(&text).unwrap_or_else(|e| panic!("parse {f:?}: {e}"));
            collect(&parsed.items, &mut macros);
        }
    }
    macros.sort();
    macros
}

/// Clause-1 scan of one in-memory source as library code of `module`.
fn scan_source(src: &str, module: &[&str]) -> Vec<Hit> {
    scan_source_with(src, module, Scan::Library, GAME_SIDE)
}

fn scan_source_with(
    src: &str,
    module: &[&str],
    scan: Scan,
    targets: &'static [&'static str],
) -> Vec<Hit> {
    let parsed = syn::parse_file(src).expect("planted source parses");
    let mut walk = Walk::new(
        Path::new(""),
        scan,
        targets,
        vec!["assert_engine_eq".to_string()],
    );
    let module: Vec<String> = module.iter().map(ToString::to_string).collect();
    let ctx = FileCtx {
        rel: "<planted>",
        dir: Path::new(""),
        children: Children::Skip,
    };
    walk.walk_items(
        &parsed.items,
        &ctx,
        Path::new(""),
        false,
        &module,
        Reach::Library,
    );
    walk.hits
}

#[test]
fn parser_library_code_names_no_game_side_module() {
    let src = engine_src();
    let macros = exported_macros(&src, GAME_SIDE);
    assert!(
        macros.iter().any(|m| m == "assert_engine_eq"),
        "reach-guard: the exported game-side macro set lost `assert_engine_eq`: {macros:?}"
    );
    let mut walk = Walk::new(&src, Scan::Library, GAME_SIDE, macros);
    walk.walk_file(
        &src.join("parser/mod.rs"),
        vec!["parser".to_string()],
        Reach::Library,
        Children::Follow,
    );

    let library = walk.files_reached(Reach::Library);
    let test_only = walk.files_reached(Reach::TestOnly);
    assert!(
        library.len() >= 80 && library.contains("parser/oracle.rs"),
        "reach-guard: the parser walk reached {} library files: {library:?}",
        library.len()
    );
    assert!(
        test_only.len() >= 10 && test_only.contains("parser/oracle_tests.rs"),
        "reach-guard: the parser walk reached {} test-only files: {test_only:?}",
        test_only.len()
    );
    let orphans: Vec<String> = rs_files(&src.join("parser"))
        .iter()
        .map(|p| walk.rel(p))
        .filter(|rel| !library.contains(rel) && !test_only.contains(rel))
        .collect();
    assert!(
        orphans.is_empty(),
        "parser files the module walk never reached, so this census cannot see them: {orphans:#?}"
    );

    let mut hits = walk.hits;
    hits.sort();
    assert!(
        hits.is_empty(),
        "parser library code names a game-side module ({GAME_SIDE:?}). Move the item it reaches \
         into the AST layer (`phase-ast`) and re-point the parser call, or keep the reference in \
         test code: {hits:#?}"
    );
}

#[test]
fn no_root_or_types_reexport_reaches_game_side() {
    let src = engine_src();
    let mut root = Walk::new(
        &src,
        Scan::Reexports(UseVisibility::Any),
        GAME_SIDE,
        Vec::new(),
    );
    root.walk_file(
        &src.join("lib.rs"),
        Vec::new(),
        Reach::Library,
        Children::Skip,
    );
    assert!(
        root.hits.is_empty(),
        "`lib.rs` imports one of {GAME_SIDE:?} through a `use`, so a crate-root path could \
         reach it from the parser: {:#?}",
        root.hits
    );

    let mut types = Walk::new(
        &src,
        Scan::Reexports(UseVisibility::NonPrivate),
        GAME_SIDE,
        Vec::new(),
    );
    types.walk_file(
        &src.join("types/mod.rs"),
        vec!["types".to_string()],
        Reach::Library,
        Children::Follow,
    );
    let reached = types.files_reached(Reach::Library);
    assert!(
        reached.len() >= 25 && reached.contains("types/ability.rs"),
        "reach-guard: the types walk reached {} library files: {reached:?}",
        reached.len()
    );
    assert!(
        types.hits.is_empty(),
        "`types` re-exports a game-side module, so a parser path through `types` would \
         cross the boundary without naming it: {:#?}",
        types.hits
    );
}

#[test]
fn the_walker_flags_every_crossing_shape_and_skips_test_code() {
    let depth2 = ["parser", "oracle_effect"];
    let one_hit = |label: &str, src: &str, module: &[&str]| {
        let hits = scan_source(src, module);
        assert_eq!(
            hits.len(),
            1,
            "{label}: expected exactly one hit, got {hits:#?}"
        );
    };
    let no_hit = |label: &str, src: &str, module: &[&str]| {
        let hits = scan_source(src, module);
        assert!(hits.is_empty(), "{label}: expected no hit, got {hits:#?}");
    };

    // Positives: each planted crossing yields exactly one hit.
    let fn_body = "fn f() { crate::game::x::y(); }";
    one_hit("crate-headed path in a fn body", fn_body, &depth2);
    one_hit(
        "super chain to the crate root",
        "fn f() { super::super::game::x(); }",
        &depth2,
    );
    one_hit("use group", "use crate::{game::x, types::y};", &depth2);
    one_hit("use glob of the crate root", "use crate::*;", &depth2);
    one_hit("use rename", "use crate::game as g;", &depth2);
    one_hit(
        "statement-level use group",
        "fn f() { use crate::{game::x, types::y}; }",
        &depth2,
    );
    one_hit(
        "statement-level use group naming the module itself",
        "fn f() { use crate::{game, types}; }",
        &depth2,
    );
    one_hit("bare game-side head", "fn f() { game::x::f(); }", &depth2);
    one_hit(
        "exported game-side macro",
        "fn f() { assert_engine_eq!(a, b); }",
        &depth2,
    );
    one_hit(
        "serde path string",
        "struct S { #[serde(default = \"crate::game::x\")] a: u8 }",
        &depth2,
    );
    one_hit(
        "cfg(any(test, feature)) is library code",
        "#[cfg(any(test, feature = \"test-support\"))] fn f() { crate::game::x::y(); }",
        &depth2,
    );
    one_hit(
        "cfg(not(test)) is library code",
        "#[cfg(not(test))] fn f() { crate::game::x::y(); }",
        &depth2,
    );
    let impl_item = "impl S { fn f() { crate::game::x(); } }";
    one_hit("impl item", impl_item, &depth2);
    one_hit(
        "non-private types re-export",
        "pub use crate::game::dungeon::DungeonId;",
        &["types", "x"],
    );

    // Negatives, each the twin of a positive above.
    no_hit(
        "cfg(test) module",
        "#[cfg(test)] mod t { fn f() { crate::game::x::y(); } }",
        &depth2,
    );
    no_hit(
        "cfg(all(test, feature))",
        "#[cfg(all(test, feature = \"x\"))] fn f() { crate::game::x::y(); }",
        &depth2,
    );
    no_hit(
        "cfg(test) impl item",
        "impl S { #[cfg(test)] fn f() { crate::game::x(); } }",
        &depth2,
    );
    no_hit(
        "comments",
        "// crate::game::x\n/// [crate::game::x]\nfn f() {}",
        &depth2,
    );
    no_hit(
        "segment that only starts with a game-side name",
        "fn f() { crate::types::game_state::X; }",
        &depth2,
    );
    no_hit(
        "super resolving below the crate root",
        "fn f() { super::game::x(); }",
        &depth2,
    );

    // Clause 2 visibility: a private `use` in `types` is not a re-export.
    let reexports = |src: &str| {
        scan_source_with(
            src,
            &["types", "x"],
            Scan::Reexports(UseVisibility::NonPrivate),
            GAME_SIDE,
        )
    };
    assert_eq!(
        reexports("pub use crate::game::dungeon::DungeonId;").len(),
        1,
        "reach-guard: the planted re-export is seen in clause-2 mode"
    );
    assert!(
        reexports("use crate::game::dungeon::DungeonId;").is_empty(),
        "a private `use` is not a re-export"
    );
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

/// Clauses 4b and 4d: every field under `crates/engine/src` and `crates/phase-ast/src` whose
/// type names `marker` is exactly `expected`, `(file, item, variant, field)`, each file
/// relative to its crate's `src/`.
fn assert_handle_held_only_by(
    marker: &str,
    expected: &[(&str, &str, Option<&str>, &str)],
    reason: &str,
) {
    let engine = engine_src();
    let ast = Path::new(env!("CARGO_MANIFEST_DIR")).join("../phase-ast/src");
    let mut hits = Vec::new();
    for src in [&engine, &ast] {
        for path in rs_files(src) {
            let text =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
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

// ---------------------------------------------------------------------------------------------
// Clause 5: parser library code names no state-layer item, and no AST item stays in the engine.
// ---------------------------------------------------------------------------------------------

/// The `types/` files that must compute as state modules: the three files A3 names for
/// Phase 4's moves. An anchor that is a façade, or no file of the crate, fails the census.
const STATE_MODULE_ANCHORS: &[&str] = &[
    "types/game_state.rs",
    "types/events.rs",
    "types/proposed_event.rs",
];

/// `serde` attribute keys whose string value names a path.
const SERDE_PATH_KEYS: &[&str] = &[
    "default",
    "skip_serializing_if",
    "with",
    "serialize_with",
    "deserialize_with",
    "from",
    "into",
    "try_from",
    "getter",
];

/// Keywords that cannot head a path. `crate`, `self` and `super` are absent: the resolver
/// handles them.
const PATH_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "else", "enum", "extern", "false", "fn", "for", "if",
    "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "Self",
    "static", "struct", "trait", "true", "type", "unsafe", "use", "where", "while", "async",
    "await", "dyn",
];

/// The resolver's recursion bound, a named fail-open limit: a re-export chain longer than
/// this resolves to nothing.
const RESOLVE_DEPTH: usize = 8;

/// Whether an index build also reads the test regions of `parser/` (clause 6).
/// Their definitions, `use` entries and units go to separate overlay maps, so a library-only
/// build and the library maps of an overlay build are the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Overlay {
    #[default]
    None,
    Tests,
}

/// Which maps a resolution reads: the library maps only (clauses 1-5), or the overlay maps
/// first and then the library maps (clause 6's test units).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Scope {
    Library,
    Tests,
}

/// The kinds of definition the index records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum DefKind {
    Struct,
    Enum,
    Union,
    Type,
    Fn,
    Const,
    Static,
    Trait,
    MacroRules,
}

/// A definition's identity. Same-named items of one kind in one file's inline modules share
/// it, as they do in the reference model (`census3.py`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ItemKey {
    file: String,
    name: String,
    kind: DefKind,
}

impl std::fmt::Display for ItemKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.file, self.name)
    }
}

/// What a written path resolves to: the items of the name it ends on, or a module.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Items(BTreeSet<ItemKey>),
    Module(Vec<String>),
    /// A path that left the index at this module (it binds no next segment: a name only
    /// `phase-ast` supplies through a façade or root glob, or one the index does not record);
    /// it names no item, and no lookup continues from it.
    Left(Vec<String>),
}

impl Target {
    fn into_refs(self) -> Vec<Ref> {
        match self {
            Target::Items(keys) => keys.into_iter().map(Ref::Item).collect(),
            Target::Module(path) => vec![Ref::Module(path)],
            Target::Left(path) => vec![Ref::Module(path)],
        }
    }
}

/// One reference a unit makes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Ref {
    Item(ItemKey),
    Module(Vec<String>),
}

impl Ref {
    fn item(&self) -> Option<&ItemKey> {
        match self {
            Ref::Item(key) => Some(key),
            Ref::Module(_) => None,
        }
    }

    /// A game-side item or module: what puts a `types/` item that names it in T.
    fn is_game_side(&self) -> bool {
        match self {
            Ref::Item(key) => is_game_side_file(&key.file),
            Ref::Module(path) => path
                .first()
                .is_some_and(|head| GAME_SIDE.contains(&head.as_str())),
        }
    }
}

impl std::fmt::Display for Ref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ref::Item(key) => write!(f, "{key}"),
            Ref::Module(path) => write!(f, "crate::{}", path.join("::")),
        }
    }
}

/// A scanned token run: a definition, a body, an impl header or member, a macro, or a parser
/// library item.
struct Unit {
    file: String,
    module: Vec<String>,
    /// The item the unit belongs to, for messages.
    item: String,
    label: String,
    tokens: Vec<String>,
    /// The signature's range in `tokens`, for an impl member.
    sig: Option<Range<usize>>,
}

/// The references one unit makes.
#[derive(Default)]
struct UnitRefs {
    /// Written paths and serde attribute strings, resolved.
    refs: BTreeSet<Ref>,
    /// The signature's references, for an impl member.
    sig_refs: BTreeSet<Ref>,
}

/// A `types/` item's units: its definitions (fields, variants, signature, type) and bodies.
#[derive(Default)]
struct ItemUnits {
    definitions: Vec<usize>,
    bodies: Vec<usize>,
}

struct ImplBlock {
    file: String,
    module: Vec<String>,
    self_path: Option<Vec<String>>,
    /// The trait's name and path tokens (generic arguments included); `None` when inherent.
    trait_: Option<(String, Vec<String>)>,
    header: Option<usize>,
    members: Vec<usize>,
    /// The `types/` items the self type resolves to.
    selves: BTreeSet<ItemKey>,
}

impl ImplBlock {
    fn in_types(&self) -> bool {
        self.file.starts_with("types/")
    }

    /// A trait impl outside `types/` is code depending on the AST, not AST behaviour, so T does
    /// not read it.
    fn is_behaviour(&self) -> bool {
        self.in_types() || self.trait_.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UseKind {
    Name,
    Glob,
}

/// One `use` leaf as written: `segs` is the written path (the prefix, for a glob), and
/// `alias` the name it binds.
#[derive(Debug, Clone)]
struct UseEntry {
    kind: UseKind,
    segs: Vec<String>,
    alias: Option<String>,
}

struct UseItem {
    file: String,
    module: Vec<String>,
    entries: Vec<UseEntry>,
}

/// An item-level macro in a `types/` file: a `macro_rules!` definition or an invocation.
struct MacroUnit {
    unit: usize,
    name: String,
    /// The structs it declares by the index's macro-struct rule: an invocation's own, or every
    /// struct a `macro_rules!`'s invocations declare.
    declares: Vec<ItemKey>,
}

/// An item-level macro invocation, held until every struct-declaring `macro_rules!` is known.
struct Invocation {
    file: String,
    module: Vec<String>,
    name: String,
    tokens: Vec<String>,
    macro_unit: Option<usize>,
}

#[derive(Default)]
struct PendingMacros {
    /// `macro_rules!` name -> the struct its body declares.
    struct_macros: BTreeMap<String, String>,
    invocations: Vec<Invocation>,
}

struct IndexCtx<'a> {
    rel: &'a str,
    dir: &'a Path,
    read: &'a dyn Fn(&str) -> Option<String>,
}

/// Clause 5's crate index: every library definition, `use`, module and impl block reachable
/// from `lib.rs`, built once and read by every set the clause computes. Game-side files
/// contribute names, `use` entries and impl blocks only.
#[derive(Default)]
struct LayerIndex {
    units: Vec<Unit>,
    items: BTreeMap<ItemKey, ItemUnits>,
    /// Every library definition's name by module, game-side and parser ones included, so a
    /// path resolves to the item it names instead of through a glob to a namesake.
    names: HashMap<Vec<String>, BTreeMap<String, BTreeSet<ItemKey>>>,
    uses: HashMap<Vec<String>, Vec<UseEntry>>,
    modules: HashSet<Vec<String>>,
    /// Library files with their module paths.
    files: Vec<(String, Vec<String>)>,
    impls: Vec<ImplBlock>,
    macros: Vec<MacroUnit>,
    parser_units: Vec<usize>,
    /// `use` items of `types/` and parser files, for the `use`-entry check.
    use_items: Vec<UseItem>,
    /// `types/` item -> the impl blocks whose self type resolves to it.
    impls_of: BTreeMap<ItemKey, Vec<usize>>,
    refs: Vec<UnitRefs>,
    resolved: RefCell<Resolutions>,
    /// Clause 6's overlay: what an `Overlay::Tests` build reads from the test regions of
    /// `parser/`. Empty for `Overlay::None`.
    overlay: Overlay,
    test_names: HashMap<Vec<String>, BTreeMap<String, BTreeSet<ItemKey>>>,
    /// Module -> its overlay `use` entries, as `(test_use_items index, entry index)`.
    test_uses: HashMap<Vec<String>, Vec<(usize, usize)>>,
    test_use_items: Vec<UseItem>,
    /// Modules declared inside test-only modules.
    test_modules: HashSet<Vec<String>>,
    test_units: Vec<TestUnit>,
    /// Overlay definition -> the test units that define it.
    test_items: BTreeMap<ItemKey, Vec<usize>>,
}

/// A test-region unit of clause 6: a test-only item, or a test-only impl member.
struct TestUnit {
    unit: Unit,
    /// A `#[test]` function, judged on its helper closure; every other unit is judged on its
    /// own references.
    is_test: bool,
}

/// Memoized resolutions: `(written path, module, scope)` -> what it resolves to.
type Resolutions = HashMap<(Vec<String>, Vec<String>, Scope), Option<Target>>;

fn is_game_side_file(file: &str) -> bool {
    let head = file.split('/').next().unwrap_or(file);
    GAME_SIDE.contains(&head.strip_suffix(".rs").unwrap_or(head))
}

fn is_ident_token(t: &str) -> bool {
    t.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// An item's attribute tokens, doc comments excluded.
fn attr_tokens(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|a| !a.path().is_ident("doc"))
        .flat_map(|a| flatten(&a.meta))
        .collect()
}

fn generics_tokens(g: &syn::Generics) -> Vec<String> {
    let mut out = flatten(g);
    out.extend(flatten(&g.where_clause));
    out
}

fn signature_tokens(sig: &syn::Signature) -> Vec<String> {
    let mut out = generics_tokens(&sig.generics);
    out.extend(flatten(&sig.inputs));
    out.extend(flatten(&sig.output));
    out
}

/// A type's written path, through references and parentheses.
fn type_path(ty: &syn::Type) -> Option<Vec<String>> {
    match ty {
        syn::Type::Path(p) if p.qself.is_none() => Some(
            p.path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect(),
        ),
        syn::Type::Reference(r) => type_path(&r.elem),
        syn::Type::Paren(p) => type_path(&p.elem),
        syn::Type::Group(g) => type_path(&g.elem),
        _ => None,
    }
}

/// The written `use` leaves of `tree`, below `prefix`.
fn use_entries(tree: &syn::UseTree, prefix: &[String], out: &mut Vec<UseEntry>) {
    let leaf = |ident: &syn::Ident| {
        let mut segs = prefix.to_vec();
        segs.push(ident.to_string());
        segs
    };
    match tree {
        syn::UseTree::Path(p) => use_entries(&p.tree, &leaf(&p.ident), out),
        syn::UseTree::Name(n) if n.ident == "self" => out.push(UseEntry {
            kind: UseKind::Name,
            segs: prefix.to_vec(),
            alias: prefix.last().cloned(),
        }),
        syn::UseTree::Name(n) => out.push(UseEntry {
            kind: UseKind::Name,
            segs: leaf(&n.ident),
            alias: Some(n.ident.to_string()),
        }),
        syn::UseTree::Rename(r) if r.ident == "self" => out.push(UseEntry {
            kind: UseKind::Name,
            segs: prefix.to_vec(),
            alias: Some(r.rename.to_string()),
        }),
        syn::UseTree::Rename(r) => out.push(UseEntry {
            kind: UseKind::Name,
            segs: leaf(&r.ident),
            alias: Some(r.rename.to_string()),
        }),
        syn::UseTree::Glob(_) => out.push(UseEntry {
            kind: UseKind::Glob,
            segs: prefix.to_vec(),
            alias: None,
        }),
        syn::UseTree::Group(g) => {
            for t in &g.items {
                use_entries(t, prefix, out);
            }
        }
    }
}

/// The paths written in `toks`: every run `a::b::c` whose head is not a field or method name
/// (after `.`) or a path continuation, and every string literal naming a `crate::` path. A run
/// that a `use` tree continues is expanded as `path_run` describes.
fn paths_in(toks: &[String]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i].as_str();
        if let Some(lit) = t.strip_prefix("<lit>") {
            if let Some(value) = lit.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                if value.starts_with("crate::") {
                    out.push(value.split("::").map(str::to_string).collect());
                }
            }
            i += 1;
            continue;
        }
        if !is_ident_token(t) || (i > 0 && matches!(toks[i - 1].as_str(), "." | "::")) {
            i += 1;
            continue;
        }
        i = path_run(toks, i, &[], &mut out);
    }
    out
}

/// Push the path run starting at the ident `toks[i]`, below `prefix`, and return the index
/// after it. A run that a `use` tree continues (a statement-level `use` inside a function, or
/// a parser `use` unit) also pushes its full paths: `a::*` as `a::*`, so a state module `a` is
/// a proper prefix, and `a::{b, c::{d}}` as `a::b` and `a::c::d`.
fn path_run(toks: &[String], i: usize, prefix: &[String], out: &mut Vec<Vec<String>>) -> usize {
    let mut segs = prefix.to_vec();
    segs.push(toks[i].clone());
    let mut j = i + 1;
    while j + 1 < toks.len() && toks[j] == "::" && is_ident_token(&toks[j + 1]) {
        segs.push(toks[j + 1].clone());
        j += 2;
    }
    if toks.get(j).is_some_and(|t| t == "::") {
        match toks.get(j + 1).map(String::as_str) {
            Some("*") => {
                segs.push("*".into());
                j += 2;
            }
            Some("<g") => {
                let end = use_group(toks, j + 2, &segs, out);
                out.push(segs);
                return end;
            }
            _ => {}
        }
    }
    out.push(segs);
    j
}

/// Push the full path of each entry of the `use` group whose first token is `toks[k]`, below
/// `prefix`, and return the index after its closing `g>`.
fn use_group(
    toks: &[String],
    mut k: usize,
    prefix: &[String],
    out: &mut Vec<Vec<String>>,
) -> usize {
    loop {
        k = match toks.get(k).map(String::as_str) {
            None => return k,
            Some("g>") => return k + 1,
            Some("<g") => use_group(toks, k + 1, prefix, out),
            // `{self}` names `prefix` itself, which the enclosing `path_run` pushes.
            Some("self") => k + 1,
            Some("*") => {
                out.push(prefix.iter().cloned().chain(["*".to_string()]).collect());
                k + 1
            }
            Some(t) if is_ident_token(t) => path_run(toks, k, prefix, out),
            Some(_) => k + 1,
        };
        // Past a rename (`as x`) to the next entry, or to the closing `g>`.
        while let Some(t) = toks.get(k) {
            match t.as_str() {
                "," => {
                    k += 1;
                    break;
                }
                "g>" => break,
                _ => k += 1,
            }
        }
    }
}

/// A path whose head is a keyword other than `crate` / `self` / `super`, or a bare `self`.
fn is_keyword_head(segs: &[String]) -> bool {
    PATH_KEYWORDS.contains(&segs[0].as_str()) || segs == ["self"]
}

/// Every string inside a `serde(…)` attribute that follows `<key> =` for a `SERDE_PATH_KEYS`
/// key and is shaped like a path, as its segments.
fn serde_paths(toks: &[String]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let opens = (0..toks.len())
        .filter(|&i| toks[i] == "serde" && toks.get(i + 1).is_some_and(|t| t == "<g"));
    for start in opens {
        let mut depth = 0usize;
        for j in start + 1..toks.len() {
            match toks[j].as_str() {
                "<g" => depth += 1,
                "g>" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                lit if toks[j - 1] == "=" && SERDE_PATH_KEYS.contains(&toks[j - 2].as_str()) => {
                    let Some(value) = lit
                        .strip_prefix("<lit>\"")
                        .and_then(|v| v.strip_suffix('"'))
                    else {
                        continue;
                    };
                    let segs: Vec<String> = value.split("::").map(str::to_string).collect();
                    let path_shaped = segs.iter().all(|s| {
                        is_ident_token(s) && s.chars().all(|c| c.is_alphanumeric() || c == '_')
                    });
                    if path_shaped {
                        out.push(segs);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// A `crate` / `self` / `super` path as an absolute module path (no `crate` head), or `None`
/// for a relative path.
fn absolutize(segs: &[String], module: &[String]) -> Option<Vec<String>> {
    match segs.first()?.as_str() {
        "crate" => Some(segs[1..].to_vec()),
        "self" => Some(module.iter().chain(&segs[1..]).cloned().collect()),
        "super" => {
            let supers = segs.iter().take_while(|s| *s == "super").count();
            let mut m = module[..module.len().saturating_sub(supers)].to_vec();
            m.extend(segs[supers..].iter().cloned());
            Some(m)
        }
        _ => None,
    }
}

/// The struct a `macro_rules!` body declares by a literal name (`struct GameState`); a
/// metavariable name (`struct $name`) declares nothing the index can name.
fn declared_struct(body: &[String]) -> Option<String> {
    body.windows(2)
        .find(|w| w[0] == "struct" && is_ident_token(&w[1]))
        .map(|w| w[1].clone())
}

/// A definition's name and kind.
fn def_ident(item: &syn::Item) -> Option<(&syn::Ident, DefKind)> {
    Some(match item {
        syn::Item::Struct(s) => (&s.ident, DefKind::Struct),
        syn::Item::Enum(e) => (&e.ident, DefKind::Enum),
        syn::Item::Union(u) => (&u.ident, DefKind::Union),
        syn::Item::Type(t) => (&t.ident, DefKind::Type),
        syn::Item::Fn(f) => (&f.sig.ident, DefKind::Fn),
        syn::Item::Const(c) => (&c.ident, DefKind::Const),
        syn::Item::Static(s) => (&s.ident, DefKind::Static),
        syn::Item::Trait(t) => (&t.ident, DefKind::Trait),
        _ => return None,
    })
}

/// A definition's tokens: attributes and definition (fields, variants, signature, type, trait
/// items), and body (a `fn` block, a `const` / `static` initializer).
fn def_tokens(item: &syn::Item) -> (Vec<String>, Vec<String>) {
    let with_attrs = |attrs: &[syn::Attribute], rest: Vec<String>| {
        let mut out = attr_tokens(attrs);
        out.extend(rest);
        out
    };
    let generic = |g: &syn::Generics, rest: Vec<String>| {
        let mut out = generics_tokens(g);
        out.extend(rest);
        out
    };
    match item {
        syn::Item::Struct(s) => (
            with_attrs(&s.attrs, generic(&s.generics, flatten(&s.fields))),
            Vec::new(),
        ),
        syn::Item::Enum(e) => (
            with_attrs(&e.attrs, generic(&e.generics, flatten(&e.variants))),
            Vec::new(),
        ),
        syn::Item::Union(u) => (
            with_attrs(&u.attrs, generic(&u.generics, flatten(&u.fields))),
            Vec::new(),
        ),
        syn::Item::Type(t) => (
            with_attrs(&t.attrs, generic(&t.generics, flatten(&t.ty))),
            Vec::new(),
        ),
        syn::Item::Fn(f) => (
            with_attrs(&f.attrs, signature_tokens(&f.sig)),
            flatten(&f.block),
        ),
        syn::Item::Const(c) => (with_attrs(&c.attrs, flatten(&c.ty)), flatten(&c.expr)),
        syn::Item::Static(s) => (with_attrs(&s.attrs, flatten(&s.ty)), flatten(&s.expr)),
        syn::Item::Trait(t) => {
            let mut rest = flatten(&t.supertraits);
            for ti in t
                .items
                .iter()
                .filter(|ti| !is_test_only(trait_item_attrs(ti)))
            {
                rest.extend(flatten(ti));
            }
            (with_attrs(&t.attrs, generic(&t.generics, rest)), Vec::new())
        }
        _ => (Vec::new(), Vec::new()),
    }
}

/// An impl member's name, its attribute, signature and body tokens in that order, and the
/// signature's range in them.
fn impl_member(item: &syn::ImplItem) -> Option<(String, Vec<String>, Range<usize>)> {
    let (name, attrs, sig, body) = match item {
        syn::ImplItem::Fn(f) => (
            f.sig.ident.to_string(),
            &f.attrs,
            signature_tokens(&f.sig),
            flatten(&f.block),
        ),
        syn::ImplItem::Const(c) => (
            c.ident.to_string(),
            &c.attrs,
            flatten(&c.ty),
            flatten(&c.expr),
        ),
        syn::ImplItem::Type(t) => (t.ident.to_string(), &t.attrs, flatten(&t.ty), Vec::new()),
        syn::ImplItem::Macro(m) => (
            "<macro>".to_string(),
            &m.attrs,
            flatten(&m.mac.tokens),
            Vec::new(),
        ),
        _ => return None,
    };
    let mut tokens = attr_tokens(attrs);
    let sig_range = tokens.len()..tokens.len() + sig.len();
    tokens.extend(sig);
    tokens.extend(body);
    Some((name, tokens, sig_range))
}

/// The kind of a game-side or state-module passage a canonical path makes, if any: a proper
/// prefix that is a game-side module or a state module.
fn passage(full: &[String], state_modules: &HashSet<Vec<String>>) -> Option<ViolationKind> {
    if full.len() < 2 {
        return None;
    }
    if GAME_SIDE.contains(&full[0].as_str()) {
        return Some(ViolationKind::GamePassage);
    }
    passes_through(full, state_modules).then_some(ViolationKind::StatePassage)
}

/// Whether a proper prefix of the canonical path `full` is one of `modules`.
fn passes_through(full: &[String], modules: &HashSet<Vec<String>>) -> bool {
    (1..full.len()).any(|k| modules.contains(&full[..k]))
}

impl LayerIndex {
    /// Index the crate whose files `read` returns by path relative to `src/`, from `lib.rs`
    /// down every `mod` declaration that is not test-only.
    fn build(read: &dyn Fn(&str) -> Option<String>, overlay: Overlay) -> Self {
        let mut index = Self {
            overlay,
            ..Self::default()
        };
        let mut pending = PendingMacros::default();
        let root = read("lib.rs").expect("the crate root `lib.rs` reads");
        index.walk_file("lib.rs", &root, Vec::new(), read, &mut pending);
        index.declare_macro_structs(pending);
        index.resolve_impl_selves();
        index.refs = (0..index.units.len()).map(|u| index.unit_refs(u)).collect();
        index
    }

    fn walk_file(
        &mut self,
        rel: &str,
        text: &str,
        module: Vec<String>,
        read: &dyn Fn(&str) -> Option<String>,
        pending: &mut PendingMacros,
    ) {
        let parsed = syn::parse_file(text).unwrap_or_else(|e| panic!("parse {rel}: {e}"));
        let path = Path::new(rel);
        let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        let child_dir = match path.file_name().and_then(|n| n.to_str()) {
            Some("mod.rs" | "lib.rs") => dir.clone(),
            _ => dir.join(path.file_stem().expect("file stem")),
        };
        self.modules.insert(module.clone());
        self.files.push((rel.to_string(), module.clone()));
        let ctx = IndexCtx {
            rel,
            dir: &dir,
            read,
        };
        self.walk_items(&parsed.items, &ctx, &child_dir, false, &module, pending);
    }

    fn walk_items(
        &mut self,
        items: &[syn::Item],
        ctx: &IndexCtx<'_>,
        child_dir: &Path,
        inline: bool,
        module: &[String],
        pending: &mut PendingMacros,
    ) {
        let overlay = self.reads_test_regions(ctx.rel);
        for item in items {
            let test = is_test_only(item_attrs(item));
            let syn::Item::Mod(m) = item else {
                if !test {
                    self.index_item(item, ctx.rel, module, pending);
                    if overlay {
                        self.index_test_members(item, ctx.rel, module);
                    }
                } else if overlay {
                    self.index_test_item(item, ctx.rel, module);
                }
                continue;
            };
            let mut path = module.to_vec();
            path.push(m.ident.to_string());
            self.modules.insert(path.clone());
            // A test-only module holds no library code, so a library build does not read its
            // files; an overlay build reads them into the overlay maps.
            if test {
                if overlay {
                    self.walk_test_mod(m, ctx, child_dir, inline, path);
                }
                continue;
            }
            match &m.content {
                Some((_, inner)) => {
                    let dir = child_dir.join(m.ident.to_string());
                    self.walk_items(inner, ctx, &dir, true, &path, pending);
                }
                None => {
                    let (rel, text) = mod_file_candidates(m, ctx.rel, ctx.dir, child_dir, inline)
                        .iter()
                        .find_map(|p| {
                            let rel = slash_path(p);
                            (ctx.read)(&rel).map(|text| (rel, text))
                        })
                        .unwrap_or_else(|| panic!("`mod {}` in {}: no file", m.ident, ctx.rel));
                    self.walk_file(&rel, &text, path, ctx.read, pending);
                }
            }
        }
    }

    fn push_unit(
        &mut self,
        file: &str,
        module: &[String],
        item: String,
        label: String,
        tokens: Vec<String>,
    ) -> usize {
        self.units.push(Unit {
            file: file.to_string(),
            module: module.to_vec(),
            item,
            label,
            tokens,
            sig: None,
        });
        self.units.len() - 1
    }

    fn add_name(&mut self, key: &ItemKey, module: &[String]) {
        self.names
            .entry(module.to_vec())
            .or_default()
            .entry(key.name.clone())
            .or_default()
            .insert(key.clone());
    }

    fn index_item(
        &mut self,
        item: &syn::Item,
        file: &str,
        module: &[String],
        pending: &mut PendingMacros,
    ) {
        let parser = file.starts_with("parser/");
        let types = file.starts_with("types/");
        match item {
            syn::Item::Use(u) => {
                let mut entries = Vec::new();
                use_entries(&u.tree, &[], &mut entries);
                if types || parser {
                    self.use_items.push(UseItem {
                        file: file.to_string(),
                        module: module.to_vec(),
                        entries: entries.clone(),
                    });
                }
                self.uses
                    .entry(module.to_vec())
                    .or_default()
                    .extend(entries);
                if parser {
                    let mut tokens = attr_tokens(&u.attrs);
                    tokens.push("use".to_string());
                    tokens.extend(flatten(&u.tree));
                    let unit = self.push_unit(file, module, "use".into(), "parser".into(), tokens);
                    self.parser_units.push(unit);
                }
            }
            syn::Item::Impl(imp) => self.index_impl(imp, file, module),
            syn::Item::Macro(m) => self.index_macro(m, file, module, pending),
            other => {
                let Some((ident, kind)) = def_ident(other) else {
                    return;
                };
                if ident == "_" {
                    return;
                }
                let key = ItemKey {
                    file: file.to_string(),
                    name: ident.to_string(),
                    kind,
                };
                self.add_name(&key, module);
                if !(types || parser) {
                    return;
                }
                let (definition, body) = def_tokens(other);
                if parser {
                    let mut tokens = definition;
                    tokens.extend(body);
                    let unit = self.push_unit(file, module, key.name, "parser".into(), tokens);
                    self.parser_units.push(unit);
                    return;
                }
                let definition = self.push_unit(
                    file,
                    module,
                    key.name.clone(),
                    "definition".into(),
                    definition,
                );
                let bodies = if body.is_empty() {
                    Vec::new()
                } else {
                    vec![self.push_unit(file, module, key.name.clone(), "body".into(), body)]
                };
                let units = self.items.entry(key).or_default();
                units.definitions.push(definition);
                units.bodies.extend(bodies);
            }
        }
    }

    fn index_impl(&mut self, imp: &syn::ItemImpl, file: &str, module: &[String]) {
        let trait_ = imp.trait_.as_ref().map(|(_, path, _)| {
            let name = path
                .segments
                .last()
                .map_or_else(String::new, |s| s.ident.to_string());
            (name, flatten(path))
        });
        let self_path = type_path(&imp.self_ty);
        let item = self_path
            .as_ref()
            .and_then(|p| p.last().cloned())
            .unwrap_or_default();
        let tr = trait_
            .as_ref()
            .map_or_else(String::new, |(name, _)| format!(" {name}"));
        // A game-side impl is read, when inherent, for T (its members).
        let game_side = is_game_side_file(file);
        let header = (!game_side).then(|| {
            let mut tokens = attr_tokens(&imp.attrs);
            tokens.extend(generics_tokens(&imp.generics));
            tokens.extend(trait_.iter().flat_map(|(_, t)| t.iter().cloned()));
            tokens.extend(flatten(&imp.self_ty));
            self.push_unit(
                file,
                module,
                item.clone(),
                format!("impl{tr} header"),
                tokens,
            )
        });
        let mut members = Vec::new();
        if !(game_side && trait_.is_some()) {
            for ii in imp
                .items
                .iter()
                .filter(|ii| !is_test_only(impl_item_attrs(ii)))
            {
                let Some((name, tokens, sig)) = impl_member(ii) else {
                    continue;
                };
                let unit = self.push_unit(
                    file,
                    module,
                    item.clone(),
                    format!("impl{tr}::{name}"),
                    tokens,
                );
                self.units[unit].sig = Some(sig);
                members.push(unit);
            }
        }
        if file.starts_with("parser/") {
            self.parser_units.extend(header.iter().chain(&members));
        }
        self.impls.push(ImplBlock {
            file: file.to_string(),
            module: module.to_vec(),
            self_path,
            trait_,
            header,
            members,
            selves: BTreeSet::new(),
        });
    }

    fn index_macro(
        &mut self,
        m: &syn::ItemMacro,
        file: &str,
        module: &[String],
        pending: &mut PendingMacros,
    ) {
        let parser = file.starts_with("parser/");
        let types = file.starts_with("types/");
        if let Some(name) = &m.ident {
            let key = ItemKey {
                file: file.to_string(),
                name: name.to_string(),
                kind: DefKind::MacroRules,
            };
            self.add_name(&key, module);
            if is_macro_export(&m.attrs) {
                self.add_name(&key, &[]);
            }
            if !(types || parser) {
                return;
            }
            let inner = flatten(&m.mac.tokens);
            if types {
                if let Some(declared) = declared_struct(&inner) {
                    pending.struct_macros.insert(name.to_string(), declared);
                }
            }
            let mut tokens = attr_tokens(&m.attrs);
            tokens.extend([
                "macro_rules".into(),
                "!".into(),
                name.to_string(),
                "<g".into(),
            ]);
            tokens.extend(inner);
            tokens.push("g>".into());
            let unit = self.push_unit(
                file,
                module,
                format!("{name}!"),
                "macro_rules".into(),
                tokens,
            );
            if parser {
                self.parser_units.push(unit);
                return;
            }
            self.items.entry(key).or_default().bodies.push(unit);
            self.macros.push(MacroUnit {
                unit,
                name: name.to_string(),
                declares: Vec::new(),
            });
            return;
        }
        if !(types || parser) {
            return;
        }
        let name = m
            .mac
            .path
            .segments
            .last()
            .map_or_else(String::new, |s| s.ident.to_string());
        let ordinal = pending
            .invocations
            .iter()
            .filter(|i| i.file == file)
            .count();
        let mut tokens = attr_tokens(&m.attrs);
        tokens.extend(flatten(&m.mac.tokens));
        let unit = self.push_unit(
            file,
            module,
            format!("{name}! #{ordinal}"),
            "macro invocation".into(),
            tokens.clone(),
        );
        if parser {
            self.parser_units.push(unit);
        }
        let macro_unit = types.then(|| {
            self.macros.push(MacroUnit {
                unit,
                name: name.clone(),
                declares: Vec::new(),
            });
            self.macros.len() - 1
        });
        pending.invocations.push(Invocation {
            file: file.to_string(),
            module: module.to_vec(),
            name,
            tokens,
            macro_unit,
        });
    }

    /// A `macro_rules!` in `types/` whose body declares `struct <Ident>` defines that struct at
    /// each invocation's module, with the invocation tokens as its definition. This is how
    /// `GameState` (`declare_game_state!`) is indexed.
    fn declare_macro_structs(&mut self, pending: PendingMacros) {
        let mut declared: BTreeMap<String, Vec<ItemKey>> = BTreeMap::new();
        for inv in pending.invocations {
            let Some(name) = pending.struct_macros.get(&inv.name) else {
                continue;
            };
            let key = ItemKey {
                file: inv.file.clone(),
                name: name.clone(),
                kind: DefKind::Struct,
            };
            self.add_name(&key, &inv.module);
            let unit = self.push_unit(
                &inv.file,
                &inv.module,
                name.clone(),
                format!("definition ({}!)", inv.name),
                inv.tokens,
            );
            self.items
                .entry(key.clone())
                .or_default()
                .definitions
                .push(unit);
            if let Some(m) = inv.macro_unit {
                self.macros[m].declares.push(key.clone());
            }
            declared.entry(inv.name).or_default().push(key);
        }
        for m in &mut self.macros {
            if self.units[m.unit].label == "macro_rules" {
                m.declares
                    .extend(declared.get(&m.name).into_iter().flatten().cloned());
            }
        }
    }

    /// Resolve every impl's self type. A game-side impl on no `types/` item feeds no set, so its
    /// member tokens are dropped before references are computed.
    fn resolve_impl_selves(&mut self) {
        let selves: Vec<BTreeSet<ItemKey>> = self
            .impls
            .iter()
            .map(|imp| {
                match imp
                    .self_path
                    .as_deref()
                    .and_then(|p| self.resolve(p, &imp.module, Scope::Library))
                {
                    Some(Target::Items(keys)) => keys
                        .into_iter()
                        .filter(|k| k.file.starts_with("types/"))
                        .collect(),
                    _ => BTreeSet::new(),
                }
            })
            .collect();
        for (i, keys) in selves.into_iter().enumerate() {
            for key in &keys {
                self.impls_of.entry(key.clone()).or_default().push(i);
            }
            if keys.is_empty() && is_game_side_file(&self.impls[i].file) {
                for &u in &self.impls[i].members {
                    self.units[u].tokens.clear();
                    self.units[u].sig = None;
                }
            }
            self.impls[i].selves = keys;
        }
    }

    fn lookup(&self, module: &[String], name: &str, depth: usize, scope: Scope) -> Option<Target> {
        if depth > RESOLVE_DEPTH {
            return None;
        }
        if let Some(keys) = self.defined(module, name, scope) {
            return Some(Target::Items(keys.clone()));
        }
        let mut child = module.to_vec();
        child.push(name.to_string());
        if self.is_module(&child, scope) {
            return Some(Target::Module(child));
        }
        let named = self
            .entries(module, scope)
            .filter(|e| e.kind == UseKind::Name && e.alias.as_deref() == Some(name))
            .find_map(|e| self.resolve_at(&e.segs, module, depth + 1, scope));
        if named.is_some() {
            return named;
        }
        self.glob_bases(module, depth, scope)
            .find_map(|base| self.lookup(&base, name, depth + 1, scope))
    }

    /// The definitions of `name` in `module`: the overlay's first under `Scope::Tests`.
    fn defined(&self, module: &[String], name: &str, scope: Scope) -> Option<&BTreeSet<ItemKey>> {
        let overlay = match scope {
            Scope::Library => None,
            Scope::Tests => self.test_names.get(module).and_then(|n| n.get(name)),
        };
        overlay.or_else(|| self.names.get(module).and_then(|n| n.get(name)))
    }

    fn is_module(&self, path: &[String], scope: Scope) -> bool {
        self.modules.contains(path) || (scope == Scope::Tests && self.test_modules.contains(path))
    }

    /// `module`'s `use` entries: under `Scope::Tests` the overlay's, then the library's.
    fn entries<'a>(
        &'a self,
        module: &[String],
        scope: Scope,
    ) -> impl Iterator<Item = &'a UseEntry> + 'a {
        let overlay = match scope {
            Scope::Library => None,
            Scope::Tests => self.test_uses.get(module),
        };
        overlay
            .into_iter()
            .flatten()
            .map(|&(item, entry)| &self.test_use_items[item].entries[entry])
            .chain(self.uses.get(module).into_iter().flatten())
    }

    /// The absolute bases of `module`'s glob `use`s, other than `module` itself.
    fn glob_bases<'a>(
        &'a self,
        module: &'a [String],
        depth: usize,
        scope: Scope,
    ) -> impl Iterator<Item = Vec<String>> + 'a {
        self.entries(module, scope)
            .filter(|e| e.kind == UseKind::Glob)
            .filter_map(move |e| self.abs_of(&e.segs, module, depth + 1, scope))
            .filter(move |base| base != module)
    }

    fn abs_of(
        &self,
        segs: &[String],
        module: &[String],
        depth: usize,
        scope: Scope,
    ) -> Option<Vec<String>> {
        if let Some(abs) = absolutize(segs, module) {
            return Some(abs);
        }
        match self.lookup(module, segs.first()?, depth + 1, scope)? {
            Target::Module(m) => Some(m.into_iter().chain(segs[1..].iter().cloned()).collect()),
            Target::Items(_) | Target::Left(_) => None,
        }
    }

    /// Resolve a written path in `module`. The longest prefix that names an item wins, so an
    /// enum variant or an associated item resolves to its type.
    fn resolve(&self, segs: &[String], module: &[String], scope: Scope) -> Option<Target> {
        let key = (segs.to_vec(), module.to_vec(), scope);
        if let Some(hit) = self.resolved.borrow().get(&key) {
            return hit.clone();
        }
        let out = self.resolve_at(segs, module, 0, scope);
        self.resolved.borrow_mut().insert(key, out.clone());
        out
    }

    fn resolve_at(
        &self,
        segs: &[String],
        module: &[String],
        depth: usize,
        scope: Scope,
    ) -> Option<Target> {
        if depth > RESOLVE_DEPTH || segs.is_empty() {
            return None;
        }
        let (base, rest) = match absolutize(segs, module) {
            Some(abs) => (Vec::new(), abs),
            None => match self.lookup(module, &segs[0], depth + 1, scope)? {
                t @ (Target::Items(_) | Target::Left(_)) => return Some(t),
                t if segs.len() == 1 => return Some(t),
                Target::Module(m) => (m, segs[1..].to_vec()),
            },
        };
        let mut cur = base;
        for s in &rest {
            match self.lookup(&cur, s, depth + 1, scope) {
                None => return (!cur.is_empty()).then_some(Target::Left(cur)),
                Some(t @ (Target::Items(_) | Target::Left(_))) => return Some(t),
                Some(Target::Module(m)) => cur = m,
            }
        }
        (!cur.is_empty()).then_some(Target::Module(cur))
    }

    /// The path the first library `use` in `module` that binds `name` names.
    fn use_binding(&self, module: &[String], name: &str, scope: Scope) -> Option<&[String]> {
        self.entries(module, scope).find_map(|e| {
            (e.kind == UseKind::Name && e.alias.as_deref() == Some(name))
                .then_some(e.segs.as_slice())
        })
    }

    /// The absolute path a written path denotes: a `crate` / `self` / `super` path absolutized,
    /// a bare head bound by a `use` replaced by that `use`'s path, a bare head naming a child
    /// module prefixed by the module path, and a bare head a glob imports (and the module does
    /// not define) prefixed by the glob's base. `None` for an extern or local name.
    fn canonical(
        &self,
        segs: &[String],
        module: &[String],
        depth: usize,
        scope: Scope,
    ) -> Option<Vec<String>> {
        if depth > RESOLVE_DEPTH {
            return None;
        }
        if let Some(abs) = absolutize(segs, module) {
            return Some(abs);
        }
        if let Some(bound) = self.use_binding(module, &segs[0], scope) {
            let base = match absolutize(bound, module) {
                Some(abs) => abs,
                None if bound.first() != segs.first() => {
                    self.canonical(bound, module, depth + 1, scope)?
                }
                None => return None,
            };
            return Some(base.into_iter().chain(segs[1..].iter().cloned()).collect());
        }
        let mut child = module.to_vec();
        child.push(segs[0].clone());
        if self.is_module(&child, scope) {
            return Some(module.iter().chain(segs).cloned().collect());
        }
        if self.defined(module, &segs[0], scope).is_some() {
            return None;
        }
        let base = self
            .glob_bases(module, depth, scope)
            .find(|base| self.resolve(&segs[..1], base, scope).is_some())?;
        Some(base.into_iter().chain(segs.iter().cloned()).collect())
    }

    fn refs_in(&self, toks: &[String], module: &[String], scope: Scope) -> BTreeSet<Ref> {
        paths_in(toks)
            .into_iter()
            .filter(|segs| !is_keyword_head(segs))
            .filter_map(|segs| {
                self.resolve(&segs, module, scope)
                    .or_else(|| self.root_macro(&segs))
            })
            .flat_map(Target::into_refs)
            .collect()
    }

    /// A bare name that resolves to nothing, taken as a crate-root macro of that name when one
    /// exists: an over-approximation, since Rust reaches such a macro bare only through an import.
    fn root_macro(&self, segs: &[String]) -> Option<Target> {
        let [name] = segs else {
            return None;
        };
        let keys: BTreeSet<ItemKey> = self
            .names
            .get(&Vec::<String>::new())?
            .get(name)?
            .iter()
            .filter(|k| k.kind == DefKind::MacroRules)
            .cloned()
            .collect();
        (!keys.is_empty()).then_some(Target::Items(keys))
    }

    fn unit_refs(&self, u: usize) -> UnitRefs {
        let unit = &self.units[u];
        let mut refs = self.refs_in(&unit.tokens, &unit.module, Scope::Library);
        for segs in serde_paths(&unit.tokens) {
            refs.extend(
                self.resolve(&segs, &unit.module, Scope::Library)
                    .into_iter()
                    .flat_map(Target::into_refs),
            );
        }
        let sig_refs = unit
            .sig
            .clone()
            .map(|range| self.refs_in(&unit.tokens[range], &unit.module, Scope::Library))
            .unwrap_or_default();
        UnitRefs { refs, sig_refs }
    }

    /// The module paths of `files`.
    fn modules_of(&self, files: &BTreeSet<String>) -> HashSet<Vec<String>> {
        self.files
            .iter()
            .filter(|(file, _)| files.contains(file))
            .map(|(_, module)| module.clone())
            .collect()
    }

    /// The game-side and state-module passages a unit's written paths and serde strings make:
    /// `(kind, written, canonical)`, one per canonical path.
    fn passages(
        &self,
        unit: &Unit,
        state_modules: &HashSet<Vec<String>>,
        scope: Scope,
    ) -> Vec<(ViolationKind, String, String)> {
        let mut seen = BTreeSet::new();
        paths_in(&unit.tokens)
            .into_iter()
            .chain(serde_paths(&unit.tokens))
            .filter(|segs| !is_keyword_head(segs))
            .filter_map(|segs| {
                let full = self.canonical(&segs, &unit.module, 0, scope)?;
                let kind = passage(&full, state_modules)?;
                let canonical = format!("crate::{}", full.join("::"));
                seen.insert(canonical.clone())
                    .then(|| (kind, segs.join("::"), canonical))
            })
            .collect()
    }
}

/// Why a `types/` item is in the state layer (T).
#[derive(Debug, Clone, PartialEq)]
enum StateWhy {
    /// Its definition names a game-side item or module, or another T item.
    Names(Ref),
    /// A member of an impl that is AST behaviour names a game-side item or module.
    Behaviour { unit: usize, reference: Ref },
    /// A member signature of such an impl names a T item.
    Signature { unit: usize, item: ItemKey },
}

/// A node of the AST closure: a `types/` item, or a macro unit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Node {
    Item(ItemKey),
    Macro(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ViolationKind {
    /// A path passes through a game-side module: `passage`'s class, reported as `TestGameSide`
    /// in a test region (clause 1 owns parser library code's).
    GamePassage,
    /// 5a: a parser path or `use` passes through a state module.
    StatePassage,
    /// 5a: parser library code names a T item.
    ParserNamesState,
    /// 5: an item of the AST closure outside T is defined in `phase-engine`'s `types/`.
    EngineAstItem,
    /// 5: a state-module anchor is a façade or no file of the crate.
    Anchor,
    /// 6: a test region names a game-side item or module, or passes through a game-side module.
    TestGameSide,
    /// 6: a test region names the crate-level card-DB loader, `crate::test_support`.
    TestSupport,
    /// 6: a test region names a T item or passes through a state module.
    TestState,
    /// 6: a test region names the crate-level source census, `crate::source_census`.
    TestSourceCensus,
    /// 6: a test region reads a `crates/engine/` source outside its own layer directory.
    TestRead,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Violation {
    kind: ViolationKind,
    file: String,
    item: String,
    label: String,
    /// What is named: a reference, a canonical path, a T item, an AST-closure item or an
    /// anchor file.
    subject: String,
    detail: String,
    witness: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} in {} `{}` ({}): {} {}",
            self.kind, self.file, self.item, self.label, self.subject, self.detail
        )?;
        if !self.witness.is_empty() {
            write!(f, " [reached: {}]", self.witness)?;
        }
        Ok(())
    }
}

/// Clause 5's sets and verdict over one crate.
struct LayerCensus {
    index: LayerIndex,
    /// `types/` items parser library code names, with the first parser unit naming each.
    seeds: BTreeMap<ItemKey, String>,
    state: BTreeMap<ItemKey, StateWhy>,
    /// The AST closure, each node with the node that reached it (`None` for a seed).
    ast: BTreeMap<Node, Option<Node>>,
    violations: Vec<Violation>,
}

impl LayerCensus {
    fn run(read: &dyn Fn(&str) -> Option<String>, anchors: &[&str], overlay: Overlay) -> Self {
        let index = LayerIndex::build(read, overlay);
        let mut census = Self {
            seeds: BTreeMap::new(),
            state: BTreeMap::new(),
            ast: BTreeMap::new(),
            violations: Vec::new(),
            index,
        };
        census.seeds = census.parser_seeds();
        census.state = census.state_items();
        census.ast = census.ast_closure();
        census.violations = census.collect_violations(anchors);
        census
    }

    fn parser_seeds(&self) -> BTreeMap<ItemKey, String> {
        let index = &self.index;
        let mut seeds = BTreeMap::new();
        for &u in &index.parser_units {
            let unit = &index.units[u];
            for key in index.refs[u].refs.iter().filter_map(Ref::item) {
                if key.file.starts_with("types/") {
                    seeds
                        .entry(key.clone())
                        .or_insert_with(|| format!("{}:{}", unit.file, unit.item));
                }
            }
        }
        seeds
    }

    /// T: the fixpoint of items whose definition names a game-side item or module or another T
    /// item, seeded by the items whose AST-behaviour impl members name a game-side item or
    /// module, and extended by those whose member signatures name a T item.
    fn state_items(&self) -> BTreeMap<ItemKey, StateWhy> {
        let index = &self.index;
        let mut state = BTreeMap::new();
        for imp in index.impls.iter().filter(|imp| imp.is_behaviour()) {
            for key in &imp.selves {
                if state.contains_key(key) {
                    continue;
                }
                let hit = imp.members.iter().find_map(|&u| {
                    index.refs[u]
                        .refs
                        .iter()
                        .find(|r| r.is_game_side())
                        .map(|r| (u, r.clone()))
                });
                if let Some((unit, reference)) = hit {
                    state.insert(key.clone(), StateWhy::Behaviour { unit, reference });
                }
            }
        }
        let def_refs: BTreeMap<&ItemKey, Vec<&Ref>> = index
            .items
            .iter()
            .map(|(key, units)| {
                let refs = units
                    .definitions
                    .iter()
                    .flat_map(|&u| &index.refs[u].refs)
                    .collect();
                (key, refs)
            })
            .collect();
        loop {
            let mut changed = false;
            for (key, refs) in &def_refs {
                if state.contains_key(*key) {
                    continue;
                }
                let hit = refs.iter().find(|r| {
                    r.is_game_side() || r.item().is_some_and(|k| k != *key && state.contains_key(k))
                });
                if let Some(r) = hit {
                    state.insert((*key).clone(), StateWhy::Names((*r).clone()));
                    changed = true;
                }
            }
            for imp in index.impls.iter().filter(|imp| imp.is_behaviour()) {
                for key in &imp.selves {
                    if state.contains_key(key) {
                        continue;
                    }
                    let hit = imp.members.iter().find_map(|&u| {
                        index.refs[u]
                            .sig_refs
                            .iter()
                            .filter_map(Ref::item)
                            .find(|k| *k != key && state.contains_key(*k))
                            .map(|k| (u, k.clone()))
                    });
                    if let Some((unit, item)) = hit {
                        state.insert(key.clone(), StateWhy::Signature { unit, item });
                        changed = true;
                    }
                }
            }
            if !changed {
                return state;
            }
        }
    }

    fn node_units(&self, node: &Node) -> Vec<usize> {
        let index = &self.index;
        match node {
            Node::Macro(m) => vec![index.macros[*m].unit],
            Node::Item(key) => {
                let mut out = Vec::new();
                if let Some(units) = index.items.get(key) {
                    out.extend(&units.definitions);
                    out.extend(&units.bodies);
                }
                for &i in index.impls_of.get(key).into_iter().flatten() {
                    let imp = &index.impls[i];
                    if imp.in_types() {
                        out.extend(imp.header);
                        out.extend(&imp.members);
                    }
                }
                out
            }
        }
    }

    /// The AST closure from the parser seeds and `macro_seeds` over unit references, stopping
    /// at T items.
    fn reach(&self, macro_seeds: &[usize]) -> BTreeMap<Node, Option<Node>> {
        let mut ast = BTreeMap::new();
        let mut queue: Vec<(Node, Option<Node>)> = self
            .seeds
            .keys()
            .map(|k| (Node::Item(k.clone()), None))
            .chain(macro_seeds.iter().map(|&m| (Node::Macro(m), None)))
            .collect();
        while let Some((node, parent)) = queue.pop() {
            if ast.contains_key(&node) {
                continue;
            }
            ast.insert(node.clone(), parent);
            for u in self.node_units(&node) {
                for key in self.index.refs[u].refs.iter().filter_map(Ref::item) {
                    let next = Node::Item(key.clone());
                    if key.file.starts_with("types/")
                        && !self.state.contains_key(key)
                        && !ast.contains_key(&next)
                    {
                        queue.push((next, Some(node.clone())));
                    }
                }
            }
        }
        ast
    }

    /// The façades: non-root `types/` files whose library `use` entries hold the glob
    /// `phase_ast::types::<the file's module>::*`.
    fn facade_files(&self) -> BTreeSet<String> {
        let index = &self.index;
        index
            .files
            .iter()
            .filter(|(file, module)| {
                file.starts_with("types/")
                    && file != "types/mod.rs"
                    && index.use_items.iter().any(|use_item| {
                        use_item.file == *file
                            && use_item.entries.iter().any(|entry| {
                                entry.kind == UseKind::Glob
                                    && entry.segs.first().is_some_and(|head| head == "phase_ast")
                                    && entry.segs[1..] == module[..]
                            })
                    })
            })
            .map(|(file, _)| file.clone())
            .collect()
    }

    /// The state modules' files: every non-root `types/` file but a façade.
    fn state_files(&self) -> BTreeSet<String> {
        let facades = self.facade_files();
        self.index
            .files
            .iter()
            .map(|(file, _)| file)
            .filter(|f| f.starts_with("types/") && *f != "types/mod.rs" && !facades.contains(*f))
            .cloned()
            .collect()
    }

    /// The closure from the parser seeds and the macro units of the `types/` files that are no
    /// state module, except a macro that declares a T item.
    fn ast_closure(&self) -> BTreeMap<Node, Option<Node>> {
        let state_files = self.state_files();
        let macro_seeds: Vec<usize> = (0..self.index.macros.len())
            .filter(|&m| {
                let unit = &self.index.units[self.index.macros[m].unit];
                !state_files.contains(&unit.file)
                    && !self.index.macros[m]
                        .declares
                        .iter()
                        .any(|k| self.state.contains_key(k))
            })
            .collect();
        self.reach(&macro_seeds)
    }

    /// Parser library paths through a façade module, one per canonical path per unit (a parser
    /// `use` is a unit, so its entries count): the reach of the façade exclusion.
    fn facade_paths(&self) -> usize {
        let index = &self.index;
        let facades = index.modules_of(&self.facade_files());
        index
            .parser_units
            .iter()
            .map(|&u| {
                let unit = &index.units[u];
                paths_in(&unit.tokens)
                    .into_iter()
                    .filter(|segs| !is_keyword_head(segs))
                    .filter_map(|segs| index.canonical(&segs, &unit.module, 0, Scope::Library))
                    .filter(|full| passes_through(full, &facades))
                    .collect::<BTreeSet<_>>()
                    .len()
            })
            .sum()
    }

    fn node_name(&self, node: &Node) -> String {
        match node {
            Node::Item(key) => key.name.clone(),
            Node::Macro(m) => self.index.units[self.index.macros[*m].unit].item.clone(),
        }
    }

    /// seed -> … -> node.
    fn ast_chain(&self, node: &Node) -> String {
        let mut chain = vec![self.node_name(node)];
        let mut cur = node;
        while let Some(Some(parent)) = self.ast.get(cur) {
            chain.push(self.node_name(parent));
            cur = parent;
        }
        let root = match cur {
            Node::Item(key) => self
                .seeds
                .get(key)
                .map_or_else(|| "seed".to_string(), |s| format!("parser {s}")),
            Node::Macro(_) => "macro unit".to_string(),
        };
        chain.push(root);
        chain.reverse();
        chain.join(" -> ")
    }

    /// item -> field or member -> … -> game-side anchor.
    fn state_chain(&self, key: &ItemKey) -> String {
        let mut chain = vec![key.name.clone()];
        let mut cur = key;
        for _ in 0..self.state.len() {
            match self.state.get(cur) {
                Some(StateWhy::Names(Ref::Item(next))) if self.state.contains_key(next) => {
                    chain.push(next.name.clone());
                    cur = next;
                }
                Some(StateWhy::Names(r)) => {
                    chain.push(r.to_string());
                    break;
                }
                Some(StateWhy::Behaviour { unit, reference }) => {
                    chain.push(format!(
                        "{} names {reference}",
                        self.index.units[*unit].label
                    ));
                    break;
                }
                Some(StateWhy::Signature { unit, item }) => {
                    chain.push(format!("{} signature", self.index.units[*unit].label));
                    chain.push(item.name.clone());
                    cur = item;
                }
                None => break,
            }
        }
        chain.join(" -> ")
    }

    fn violation(
        &self,
        kind: ViolationKind,
        unit: &Unit,
        subject: String,
        detail: String,
        witness: String,
    ) -> Violation {
        Violation {
            kind,
            file: unit.file.clone(),
            item: unit.item.clone(),
            label: unit.label.clone(),
            subject,
            detail,
            witness,
        }
    }

    fn collect_violations(&self, anchors: &[&str]) -> Vec<Violation> {
        let index = &self.index;
        let state_files = self.state_files();
        let state_modules = index.modules_of(&state_files);
        let mut out = Vec::new();

        // The AST closure holds no item outside T: such an item stayed in `phase-engine`.
        for node in self.ast.keys() {
            let (file, item, subject) = match node {
                Node::Item(key) if self.state.contains_key(key) => continue,
                Node::Item(key) => (key.file.clone(), key.name.clone(), key.to_string()),
                Node::Macro(m) => {
                    let unit = &index.units[index.macros[*m].unit];
                    (unit.file.clone(), unit.item.clone(), unit.item.clone())
                }
            };
            out.push(Violation {
                kind: ViolationKind::EngineAstItem,
                file,
                item,
                label: "AST closure".into(),
                subject,
                detail: "is an AST item outside `phase-ast`".into(),
                witness: self.ast_chain(node),
            });
        }

        // 5a: `use` entries of parser library code pass through no state module (clause 1
        // owns game-side ones).
        let use_subject = |full: &[String]| format!("crate::{}", full[..full.len() - 1].join("::"));
        for use_item in index
            .use_items
            .iter()
            .filter(|use_item| use_item.file.starts_with("parser/"))
        {
            for entry in &use_item.entries {
                let mut probe = entry.segs.clone();
                probe.push("_".into());
                let Some(full) = index
                    .canonical(&probe, &use_item.module, 0, Scope::Library)
                    .filter(|full| {
                        passage(full, &state_modules) == Some(ViolationKind::StatePassage)
                    })
                else {
                    continue;
                };
                out.push(Violation {
                    kind: ViolationKind::StatePassage,
                    file: use_item.file.clone(),
                    item: "<use>".into(),
                    label: "use".into(),
                    subject: use_subject(&full),
                    detail: format!("(written `{}`)", entry.segs.join("::")),
                    witness: String::new(),
                });
            }
        }

        // 5a: parser library units name no T item and pass through no state module.
        for &u in &index.parser_units {
            let unit = &index.units[u];
            for (kind, written, canonical) in index.passages(unit, &state_modules, Scope::Library) {
                if kind == ViolationKind::StatePassage {
                    out.push(self.violation(
                        kind,
                        unit,
                        canonical,
                        format!("(written `{written}`)"),
                        String::new(),
                    ));
                }
            }
            for key in index.refs[u].refs.iter().filter_map(Ref::item) {
                if self.state.contains_key(key) {
                    out.push(self.violation(
                        ViolationKind::ParserNamesState,
                        unit,
                        key.to_string(),
                        format!("({})", self.state_chain(key)),
                        String::new(),
                    ));
                }
            }
        }

        // The state-module anchors.
        for anchor in anchors {
            if state_files.contains(*anchor) {
                continue;
            }
            let found = index.files.iter().any(|(file, _)| file == anchor);
            out.push(Violation {
                kind: ViolationKind::Anchor,
                file: anchor.to_string(),
                item: "anchor".into(),
                label: "state module".into(),
                subject: anchor.to_string(),
                detail: if found {
                    "is a façade".to_string()
                } else {
                    "is not a file of the crate".to_string()
                },
                witness: String::new(),
            });
        }

        out.sort();
        out.dedup();
        out
    }
}

/// The floor of `LayerCensus::facade_paths` over the crate: 90% of the count measured when
/// `phase-ast` was extracted (N = 24233).
const FACADE_PATHS_FLOOR: usize = 21_809;

#[test]
fn parser_library_code_names_no_state_layer_item() {
    let src = engine_src();
    let census = LayerCensus::run(
        &|rel: &str| std::fs::read_to_string(src.join(rel)).ok(),
        STATE_MODULE_ANCHORS,
        Overlay::None,
    );

    // Violations report first, so a crossing that also moves a reach-guard anchor prints its
    // witness before any guard runs.
    let listed: Vec<String> = census.violations.iter().map(ToString::to_string).collect();
    assert!(
        listed.is_empty(),
        "parser library code names the state layer, or an AST item is still in `phase-engine`. \
         Re-point the path to the item's `phase-ast` home (through its façade), move the \
         AST-shaped item to `phase-ast`, or turn a state-reading method into an extension trait \
         in the owning game module:\n{}",
        listed.join("\n")
    );

    let in_state = |file: &str, name: &str| {
        census
            .state
            .keys()
            .any(|k| k.file == file && k.name == name)
    };
    for (file, name) in [
        ("types/ability.rs", "ResolvedAbility"),
        ("types/game_state.rs", "GameState"),
        ("types/player.rs", "Player"),
        ("types/mana.rs", "ManaPool"),
    ] {
        assert!(
            in_state(file, name),
            "reach-guard: `{file}:{name}` is not in the state layer"
        );
    }
    let facade_paths = census.facade_paths();
    assert!(
        census.state.len() >= 300 && facade_paths >= FACADE_PATHS_FLOOR,
        "reach-guard: state-layer items {}, parser paths through a façade {facade_paths}",
        census.state.len()
    );
}

/// The planted crate for clause 5's and clause 6's controls: `(path relative to src/,
/// source)`. Clause 6's test regions are test-only, so clause 5 sees none of them.
const PLANTED_CRATE: &[(&str, &str)] = &[
    (
        "lib.rs",
        "pub mod game; pub mod parser; pub mod types;
         #[cfg(test)] mod test_support;
         #[cfg(test)] mod source_census;",
    ),
    (
        "game/mod.rs",
        "pub mod obj;
         pub mod glob_mod { pub trait GlobExt { fn g(&self); } }
         // A game-side test region is out of clause 6's scope.
         #[cfg(test)]
         mod tests { #[test] fn game_names_parser() { crate::parser::seeds(); } }
         pub trait LocalExt { fn ext(&self); }",
    ),
    ("game/obj.rs", "pub struct Obj; pub fn touch() {}"),
    (
        "types/mod.rs",
        "pub mod ast; pub mod deep; pub mod game_state; pub mod state;
         pub use phase_ast::types::*;
         pub use state::Runtime;",
    ),
    (
        "types/state.rs",
        "pub use super::deep::Mode;
         pub struct Runtime { pub obj: crate::game::obj::Obj }",
    ),
    (
        "types/deep.rs",
        "pub enum Mode { A, B }
         pub struct Handle { pub obj: crate::game::obj::Obj }
         pub fn make_handle() -> Handle { Handle { obj: crate::game::obj::Obj } }",
    ),
    (
        "types/game_state.rs",
        "pub struct Ledger { pub runtime: super::state::Runtime }",
    ),
    (
        "types/ast.rs",
        r#"// A façade: the engine half of a file whose AST half is in `phase-ast`.
        pub use phase_ast::types::ast::*;
        // A types-located inherent impl member and trait impl member naming a T item in their
        // bodies put neither self type in T.
        pub struct WithInherent;
        impl WithInherent { pub fn h(&self) { super::deep::make_handle(); } }
        pub struct WithTrait;
        impl Default for WithTrait { fn default() -> Self { super::deep::make_handle(); Self } }
        // A macro-declared struct in the state layer: neither macro seeds the AST closure.
        macro_rules! declare_session { ($($f:tt)*) => { pub struct Session { $($f)* } }; }
        declare_session! { pub obj: crate::game::obj::Obj }
        // Items embedding a state-layer struct.
        pub enum VariantEmbeds { V(super::deep::Handle) }
        pub struct FieldEmbeds { pub h: crate::types::deep::Handle }
        // An item outside T the parser names, and the helper only its serde string reaches.
        pub struct Leftover { #[serde(default = "leftover_default")] pub n: u8 }
        fn leftover_default() -> u8 { 0 }
        "#,
    ),
    (
        "parser/mod.rs",
        "pub mod ctl; pub mod filemod; pub mod p;
         // `Moved` is a name only the façade's `phase_ast` glob supplies.
         pub fn seeds() {
             let _: Option<(
                 crate::types::ast::Session, crate::types::ast::VariantEmbeds,
                 crate::types::ast::FieldEmbeds, crate::types::ast::Leftover,
                 crate::types::ast::Moved,
             )> = None;
         }
         pub fn via_root() -> Option<crate::types::Runtime> { None }
         pub fn via_state() -> Option<crate::types::state::Mode> { None }
         use crate::types::gone::Gone;
         pub fn via_left() { let _ = Gone::Runtime; }
         use crate::types::{self as tys};
         pub fn via_alias() -> Option<tys::Runtime> { None }
         // A test-only definition shadowing, under `Scope::Tests` only, the name a glob supplies
         // library code: clause 5 resolves `Handle` to the state-layer one.
         pub mod shadow {
             use crate::types::deep::*;
             #[cfg(test)]
             pub struct Handle;
             pub struct UsesHandle { pub h: Handle }
         }
         #[cfg(test)]
         fn test_only() -> Option<crate::types::state::Runtime> { None }",
    ),
    (
        "parser/ctl.rs",
        r#"// Library code clause 1 sees, not a test region.
        #[cfg(any(test, feature = "test-support"))]
        pub mod th { pub fn library_helper() { crate::game::obj::touch(); } }
        #[cfg(test)]
        mod tests {
            use crate::game::obj::Obj;
            use crate::types::gone::Gone;
            #[test] fn left_binding() { let _ = Gone::Runtime; }
            #[test] fn direct_path() { crate::game::obj::touch(); }
            #[test] fn use_bound() { let _ = Obj; }
            fn reaches_game() { crate::game::obj::touch(); }
            #[test] fn through_helper() { reaches_game(); }
            #[test] fn names_test_support() { crate::test_support::shared_card_db(); }
            #[test] fn names_source_census() { crate::source_census::code(); }
            #[test] fn state_item() { let _: Option<crate::types::state::Runtime> = None; }
            #[test] fn reads_ast_file() { let _ = include_str!("../types/x.rs"); }
            #[test]
            fn reads_manifest_game() {
                let _ = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/game/x.rs"));
            }
            #[test] fn reads_manifest_bare() { let _ = env!("CARGO_MANIFEST_DIR"); }
            #[test] fn reads_self() { let _ = include_str!("ctl.rs"); }
            #[test]
            fn reads_client() {
                let _ = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../client/x.json"));
            }
            mod inner { use super::*; #[test] fn through_glob() { let _ = Obj; } }
        }
        // A trait and a glob reached only through a `use` entry, called by method syntax.
        #[cfg(test)]
        mod trait_shape { use crate::game::LocalExt; #[test] fn method_call() { 0u8.ext(); } }
        #[cfg(test)]
        mod glob_entry { use crate::game::glob_mod::*; #[test] fn glob_method() { 0u8.g(); } }
        "#,
    ),
    ("parser/filemod.rs", "#[cfg(test)] mod tests;"),
    (
        "parser/filemod/tests.rs",
        "#[test] fn in_file() { crate::game::obj::touch(); }",
    ),
    (
        "parser/p.rs",
        r#"#[cfg(test)]
        fn p_helper() { crate::game::obj::touch(); }
        #[cfg(test)]
        #[path = "p_tests.rs"]
        mod tests;
        "#,
    ),
    (
        "parser/p_tests.rs",
        "use super::*; #[test] fn t() { p_helper(); }",
    ),
];

#[test]
fn the_layer_census_resolves_every_reference_shape() {
    let files: HashMap<&str, &str> = PLANTED_CRATE.iter().copied().collect();
    let read = |rel: &str| files.get(rel).map(|s| s.to_string());
    let anchors = [
        "types/game_state.rs",
        "types/state.rs",
        "types/ast.rs",
        "types/gone.rs",
    ];
    let census = LayerCensus::run(&read, &anchors, Overlay::None);

    // Non-leak: clause 6's overlay changes none of clause 5's sets. `UsesHandle` is the live
    // shape: resolved with `Scope::Tests`, its `Handle` is the test-only one.
    let uses_handle = |c: &LayerCensus| {
        c.violations.iter().any(|v| {
            v.kind == ViolationKind::ParserNamesState
                && v.item == "UsesHandle"
                && v.subject == "types/deep.rs:Handle"
        })
    };
    assert!(
        uses_handle(&census),
        "clause 5 resolves `shadow::UsesHandle`'s `Handle` through the glob"
    );
    let overlaid = LayerCensus::run(&read, &anchors, Overlay::Tests);
    assert!(
        overlaid.state == census.state
            && overlaid.ast == census.ast
            && overlaid.violations == census.violations,
        "an overlay build changes clause 5's state layer, AST closure or violations"
    );

    let in_state = |name: &str| census.state.keys().any(|k| k.name == name);
    for name in [
        "Handle",
        "make_handle",
        "Runtime",
        "Ledger",
        "Session",
        "VariantEmbeds",
        "FieldEmbeds",
    ] {
        assert!(
            in_state(name),
            "`{name}` is in the state layer: {:#?}",
            census.state.keys()
        );
    }
    for name in [
        "WithInherent",
        "WithTrait",
        "Mode",
        "Leftover",
        "leftover_default",
    ] {
        assert!(!in_state(name), "`{name}` is not in the state layer");
    }

    let mut expected: Vec<(ViolationKind, &str, &str)> = vec![
        // 5a: a T item named directly, through the `types` root re-export, and through a glob.
        (
            ViolationKind::ParserNamesState,
            "seeds",
            "types/ast.rs:Session",
        ),
        (
            ViolationKind::ParserNamesState,
            "seeds",
            "types/ast.rs:VariantEmbeds",
        ),
        (
            ViolationKind::ParserNamesState,
            "seeds",
            "types/ast.rs:FieldEmbeds",
        ),
        (
            ViolationKind::ParserNamesState,
            "via_root",
            "types/state.rs:Runtime",
        ),
        (
            ViolationKind::ParserNamesState,
            "via_alias",
            "types/state.rs:Runtime",
        ),
        (
            ViolationKind::ParserNamesState,
            "UsesHandle",
            "types/deep.rs:Handle",
        ),
        // 5a: state-module passages, in a path, through a glob, in a `use` unit and in the
        // `use` entry; a path through the façade is none.
        (
            ViolationKind::StatePassage,
            "via_state",
            "crate::types::state::Mode",
        ),
        (
            ViolationKind::StatePassage,
            "UsesHandle",
            "crate::types::deep::Handle",
        ),
        (ViolationKind::StatePassage, "use", "crate::types::deep::*"),
        (ViolationKind::StatePassage, "<use>", "crate::types::deep"),
        // The AST closure: items outside T the parser names, and one reached through a serde
        // string.
        (ViolationKind::EngineAstItem, "Mode", "types/deep.rs:Mode"),
        (
            ViolationKind::EngineAstItem,
            "Leftover",
            "types/ast.rs:Leftover",
        ),
        (
            ViolationKind::EngineAstItem,
            "leftover_default",
            "types/ast.rs:leftover_default",
        ),
        // The anchors: a façade, and a path that is no file.
        (ViolationKind::Anchor, "anchor", "types/ast.rs"),
        (ViolationKind::Anchor, "anchor", "types/gone.rs"),
    ];
    expected.sort();
    let mut actual: Vec<(ViolationKind, &str, &str)> = census
        .violations
        .iter()
        .map(|v| (v.kind, v.item.as_str(), v.subject.as_str()))
        .collect();
    actual.sort();
    actual.dedup();
    let listed: Vec<String> = census.violations.iter().map(ToString::to_string).collect();
    assert_eq!(
        actual,
        expected,
        "planted violations:\n{}",
        listed.join("\n")
    );
    assert_eq!(
        census.index.resolve(
            &["Gone".to_string(), "Runtime".to_string()],
            &["parser".to_string()],
            Scope::Library
        ),
        Some(Target::Left(vec!["types".to_string()]))
    );

    for (item, chain) in [
        (
            "types/ast.rs:VariantEmbeds",
            "VariantEmbeds -> Handle -> game/obj.rs:Obj",
        ),
        (
            "types/ast.rs:FieldEmbeds",
            "FieldEmbeds -> Handle -> game/obj.rs:Obj",
        ),
    ] {
        let found = census.violations.iter().any(|v| {
            v.kind == ViolationKind::ParserNamesState
                && v.subject == item
                && v.detail.contains(chain)
        });
        assert!(
            found,
            "the 5a witness for `{item}` reads `{chain}`:\n{}",
            listed.join("\n")
        );
    }
    for (item, chain) in [
        ("Mode", "parser parser/mod.rs:via_state -> Mode"),
        (
            "leftover_default",
            "parser parser/mod.rs:seeds -> Leftover -> leftover_default",
        ),
    ] {
        let found = census.violations.iter().any(|v| {
            v.kind == ViolationKind::EngineAstItem && v.item == item && v.witness == chain
        });
        assert!(
            found,
            "the AST-closure witness for `{item}` reads `{chain}`:\n{}",
            listed.join("\n")
        );
    }
    // The façade paths: the five `seeds` names, `Moved` included.
    assert_eq!(census.facade_paths(), 5, "parser paths through the façade");
}

// ---------------------------------------------------------------------------------------------
// Clause 6: test regions of `parser/` files cross no future crate boundary.
// ---------------------------------------------------------------------------------------------

/// Clause-6 exemptions: `(file, unit, kind, subject, reason)`. Each row must match a hit, so a
/// row that outlives its reference fails the census instead of silently widening it.
const TEST_REGION_EXEMPTIONS: &[(&str, &str, ViolationKind, &str, &str)] = &[];

/// How clause 6 classifies a source read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ReadClass {
    /// A `crates/engine/` path outside the reading unit's layer directory, or a
    /// `CARGO_MANIFEST_DIR` with no adjacent literal.
    Crossing,
    /// A path inside the reading unit's layer directory (`src/parser/`).
    InLayer,
    /// A path outside `crates/engine/`.
    OutOfCrate,
}

/// One reference-level hit of a test unit: its kind, subject and detail.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TestHit {
    kind: ViolationKind,
    subject: String,
    detail: String,
}

/// Clause 6's verdict over an overlay index.
struct TestRegionCensus {
    violations: Vec<Violation>,
    /// Exemption rows that matched no hit.
    stale: Vec<String>,
    parser_tests: usize,
    /// Reads classified as not crossing: `(file, unit, class, path)`.
    benign_reads: Vec<(String, String, ReadClass, String)>,
}

/// `rel` against `base`, both `/`-separated, with `.` and `..` folded.
fn normalize_path(base: &str, rel: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in base.split('/').chain(rel.split('/')) {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

fn str_literal(token: &str) -> Option<String> {
    let lit = token.strip_prefix("<lit>")?;
    syn::parse_str::<syn::LitStr>(lit).ok().map(|s| s.value())
}

/// The source reads in `toks`, a unit of `file` (relative to `src/`): an `include_str!` /
/// `include_bytes!` literal, resolved against the file's directory, or an
/// `env!("CARGO_MANIFEST_DIR")` followed by a literal, resolved against `crates/engine/`.
/// `None` is a `CARGO_MANIFEST_DIR` with no adjacent literal. Paths are from the repository
/// root.
fn reads_in(toks: &[String], file: &str) -> Vec<Option<String>> {
    let dir = Path::new(file)
        .parent()
        .map_or_else(String::new, slash_path);
    let file_base = format!("crates/engine/src/{dir}");
    let mut out = Vec::new();
    for i in 0..toks.len() {
        let opens = |name: &str| {
            toks[i] == name
                && toks.get(i + 1).is_some_and(|t| t == "!")
                && toks.get(i + 2).is_some_and(|t| t == "<g")
        };
        if opens("include_str") || opens("include_bytes") {
            if let Some(lit) = toks.get(i + 3).and_then(|t| str_literal(t)) {
                out.push(Some(normalize_path(&file_base, &lit)));
            }
        }
        let manifest = opens("env")
            && toks.get(i + 3).and_then(|t| str_literal(t)).as_deref()
                == Some("CARGO_MANIFEST_DIR")
            && toks.get(i + 4).is_some_and(|t| t == "g>");
        if manifest {
            let next = toks[i + 5..].iter().find(|t| *t != ",");
            out.push(
                next.and_then(|t| str_literal(t))
                    .map(|lit| normalize_path("crates/engine", &lit)),
            );
        }
    }
    out
}

/// A token run with `#[doc = …]` / `#![doc = …]` attributes removed, so doc text is no
/// reference.
fn without_docs(toks: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        let bang = usize::from(toks.get(i + 1).is_some_and(|t| t == "!"));
        let is_doc = toks[i] == "#"
            && toks.get(i + 1 + bang).is_some_and(|t| t == "<g")
            && toks.get(i + 2 + bang).is_some_and(|t| t == "doc")
            && toks.get(i + 3 + bang).is_some_and(|t| t == "=")
            && toks.get(i + 5 + bang).is_some_and(|t| t == "g>");
        if is_doc {
            i += 6 + bang;
        } else {
            out.push(toks[i].clone());
            i += 1;
        }
    }
    out
}

fn is_test_fn(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("test"))
}

impl LayerIndex {
    /// An overlay build reads the test regions of `parser/` files.
    fn reads_test_regions(&self, file: &str) -> bool {
        self.overlay == Overlay::Tests && file.starts_with("parser/")
    }

    fn push_test_unit(&mut self, unit: Unit, is_test: bool) -> usize {
        self.test_units.push(TestUnit { unit, is_test });
        self.test_units.len() - 1
    }

    /// The test regions of a library item, each a test unit: a `#[test]` fn with no test-only
    /// `cfg` (rustc builds it only under `--test`), or a test-only member of an impl.
    fn index_test_members(&mut self, item: &syn::Item, file: &str, module: &[String]) {
        let imp = match item {
            syn::Item::Fn(f) if is_test_fn(&f.attrs) => {
                let unit = Unit {
                    file: file.to_string(),
                    module: module.to_vec(),
                    item: f.sig.ident.to_string(),
                    label: "test".into(),
                    tokens: without_docs(flatten(item)),
                    sig: None,
                };
                self.push_test_unit(unit, true);
                return;
            }
            syn::Item::Impl(imp) => imp,
            _ => return,
        };
        for ii in imp
            .items
            .iter()
            .filter(|ii| is_test_only(impl_item_attrs(ii)))
        {
            let Some((name, tokens, _)) = impl_member(ii) else {
                continue;
            };
            let is_test = matches!(ii, syn::ImplItem::Fn(f) if is_test_fn(&f.attrs));
            let unit = Unit {
                file: file.to_string(),
                module: module.to_vec(),
                item: name,
                label: "test impl member".into(),
                tokens,
                sig: None,
            };
            self.push_test_unit(unit, is_test);
        }
    }

    /// A test-only item into the overlay: its definition, `use` entries and unit.
    fn index_test_item(&mut self, item: &syn::Item, file: &str, module: &[String]) {
        if let syn::Item::Use(u) = item {
            let mut entries = Vec::new();
            use_entries(&u.tree, &[], &mut entries);
            let at = self.test_use_items.len();
            self.test_uses
                .entry(module.to_vec())
                .or_default()
                .extend((0..entries.len()).map(|e| (at, e)));
            self.test_use_items.push(UseItem {
                file: file.to_string(),
                module: module.to_vec(),
                entries,
            });
            return;
        }
        let (name, key, is_test) = match item {
            syn::Item::Impl(imp) => {
                let name = type_path(&imp.self_ty)
                    .and_then(|p| p.last().cloned())
                    .unwrap_or_default();
                (format!("impl {name}"), None, false)
            }
            syn::Item::Macro(m) => match &m.ident {
                Some(ident) => (
                    format!("{ident}!"),
                    Some((ident.to_string(), DefKind::MacroRules)),
                    false,
                ),
                None => {
                    let name = m
                        .mac
                        .path
                        .segments
                        .last()
                        .map_or_else(String::new, |s| s.ident.to_string());
                    (format!("{name}!"), None, false)
                }
            },
            other => {
                let Some((ident, kind)) = def_ident(other) else {
                    return;
                };
                let is_test = matches!(other, syn::Item::Fn(f) if is_test_fn(&f.attrs));
                (ident.to_string(), Some((ident.to_string(), kind)), is_test)
            }
        };
        let unit = Unit {
            file: file.to_string(),
            module: module.to_vec(),
            item: name,
            label: if is_test { "test" } else { "test helper" }.into(),
            tokens: without_docs(flatten(item)),
            sig: None,
        };
        let u = self.push_test_unit(unit, is_test);
        if let Some((name, kind)) = key.filter(|(n, _)| n != "_") {
            let key = ItemKey {
                file: file.to_string(),
                name: name.clone(),
                kind,
            };
            self.test_names
                .entry(module.to_vec())
                .or_default()
                .entry(name)
                .or_default()
                .insert(key.clone());
            self.test_items.entry(key).or_default().push(u);
        }
    }

    /// A test-only module into the overlay, inline or through its file.
    fn walk_test_mod(
        &mut self,
        m: &syn::ItemMod,
        ctx: &IndexCtx<'_>,
        child_dir: &Path,
        inline: bool,
        path: Vec<String>,
    ) {
        match &m.content {
            Some((_, inner)) => {
                let dir = child_dir.join(m.ident.to_string());
                self.walk_test_items(inner, ctx, &dir, true, &path);
            }
            None => {
                let (rel, text) = mod_file_candidates(m, ctx.rel, ctx.dir, child_dir, inline)
                    .iter()
                    .find_map(|p| {
                        let rel = slash_path(p);
                        (ctx.read)(&rel).map(|text| (rel, text))
                    })
                    .unwrap_or_else(|| panic!("`mod {}` in {}: no file", m.ident, ctx.rel));
                let parsed = syn::parse_file(&text).unwrap_or_else(|e| panic!("parse {rel}: {e}"));
                let file = Path::new(&rel);
                let dir = file.parent().unwrap_or(Path::new("")).to_path_buf();
                let file_child_dir = match file.file_name().and_then(|n| n.to_str()) {
                    Some("mod.rs" | "lib.rs") => dir.clone(),
                    _ => dir.join(file.file_stem().expect("file stem")),
                };
                let file_ctx = IndexCtx {
                    rel: &rel,
                    dir: &dir,
                    read: ctx.read,
                };
                self.walk_test_items(&parsed.items, &file_ctx, &file_child_dir, false, &path);
            }
        }
    }

    fn walk_test_items(
        &mut self,
        items: &[syn::Item],
        ctx: &IndexCtx<'_>,
        child_dir: &Path,
        inline: bool,
        module: &[String],
    ) {
        for item in items {
            match item {
                syn::Item::Mod(m) => {
                    let mut path = module.to_vec();
                    path.push(m.ident.to_string());
                    self.test_modules.insert(path.clone());
                    self.walk_test_mod(m, ctx, child_dir, inline, path);
                }
                other => self.index_test_item(other, ctx.rel, module),
            }
        }
    }
}

impl LayerCensus {
    /// The clause-6 hits of one reference of a test unit.
    fn test_ref_hit(&self, r: &Ref) -> Option<TestHit> {
        let head = match r {
            Ref::Item(key) => key.file.split(['/', '.']).next().unwrap_or_default(),
            Ref::Module(path) => path.first().map_or("", String::as_str),
        };
        let (kind, detail) = if r.is_game_side() {
            (ViolationKind::TestGameSide, "is game-side".to_string())
        } else if head == "test_support" {
            (
                ViolationKind::TestSupport,
                "is the crate-level card-DB loader".into(),
            )
        } else if head == "source_census" {
            (
                ViolationKind::TestSourceCensus,
                "is the crate-level source census".into(),
            )
        } else {
            let key = r.item().filter(|k| self.state.contains_key(*k))?;
            (
                ViolationKind::TestState,
                format!("is a state-layer item ({})", self.state_chain(key)),
            )
        };
        Some(TestHit {
            kind,
            subject: r.to_string(),
            detail,
        })
    }

    /// The clause-6 kind of a passage a test unit makes.
    fn test_passage_kind(kind: ViolationKind) -> Option<ViolationKind> {
        match kind {
            ViolationKind::GamePassage => Some(ViolationKind::TestGameSide),
            ViolationKind::StatePassage => Some(ViolationKind::TestState),
            _ => None,
        }
    }

    /// A test unit's own hits, the overlay units it names (its helper edges), and its reads.
    fn test_unit_hits(
        &self,
        u: usize,
        state_modules: &HashSet<Vec<String>>,
        benign: &mut Vec<(String, String, ReadClass, String)>,
    ) -> (BTreeSet<TestHit>, BTreeSet<usize>) {
        let index = &self.index;
        let unit = &index.test_units[u].unit;
        let layer = unit.file.split('/').next().unwrap_or_default();
        let mut hits = BTreeSet::new();
        let mut edges = BTreeSet::new();
        for r in index.refs_in(&unit.tokens, &unit.module, Scope::Tests) {
            if let Some(defs) = r.item().and_then(|k| index.test_items.get(k)) {
                edges.extend(defs.iter().copied().filter(|&d| d != u));
            }
            hits.extend(self.test_ref_hit(&r));
        }
        for (kind, written, canonical) in index.passages(unit, state_modules, Scope::Tests) {
            if let Some(kind) = Self::test_passage_kind(kind) {
                hits.insert(TestHit {
                    kind,
                    subject: canonical,
                    detail: format!("(written `{written}`)"),
                });
            }
        }
        let layer_dir = format!("crates/engine/src/{layer}/");
        for read in reads_in(&unit.tokens, &unit.file) {
            let class = match &read {
                None => ReadClass::Crossing,
                Some(p) if !p.starts_with("crates/engine/") => ReadClass::OutOfCrate,
                Some(p) if p.starts_with(&layer_dir) => ReadClass::InLayer,
                Some(_) => ReadClass::Crossing,
            };
            match (class, read) {
                (ReadClass::Crossing, Some(p)) => {
                    hits.insert(TestHit {
                        kind: ViolationKind::TestRead,
                        subject: p.trim_start_matches("crates/engine/").to_string(),
                        detail: format!("reads outside `src/{layer}/`"),
                    });
                }
                (ReadClass::Crossing, None) => {
                    hits.insert(TestHit {
                        kind: ViolationKind::TestRead,
                        subject: "env!(\"CARGO_MANIFEST_DIR\")".into(),
                        detail: "with no adjacent literal (fail closed)".into(),
                    });
                }
                (class, Some(p)) => benign.push((unit.file.clone(), unit.item.clone(), class, p)),
                (_, None) => unreachable!("a read with no path is crossing"),
            }
        }
        (hits, edges)
    }

    /// Clause 6 over an `Overlay::Tests` build, with `exemptions` applied and stale-checked.
    fn test_regions(
        &self,
        exemptions: &[(&str, &str, ViolationKind, &str, &str)],
    ) -> TestRegionCensus {
        let index = &self.index;
        assert_eq!(
            index.overlay,
            Overlay::Tests,
            "clause 6 reads an overlay build"
        );
        let state_modules = index.modules_of(&self.state_files());

        let mut benign = Vec::new();
        let (own, edges): (Vec<BTreeSet<TestHit>>, Vec<BTreeSet<usize>>) =
            (0..index.test_units.len())
                .map(|u| self.test_unit_hits(u, &state_modules, &mut benign))
                .unzip();

        // Each `#[test]`'s closure over helper edges, each unit with the unit that reached it.
        let tests: Vec<usize> = (0..index.test_units.len())
            .filter(|&u| index.test_units[u].is_test)
            .collect();
        let closures: BTreeMap<usize, BTreeMap<usize, Option<usize>>> = tests
            .iter()
            .map(|&t| {
                let mut seen = BTreeMap::new();
                let mut queue = vec![(t, None)];
                while let Some((u, parent)) = queue.pop() {
                    if seen.contains_key(&u) {
                        continue;
                    }
                    seen.insert(u, parent);
                    queue.extend(edges[u].iter().map(|&v| (v, Some(u))));
                }
                (t, seen)
            })
            .collect();
        let chain = |closure: &BTreeMap<usize, Option<usize>>, mut u: usize| {
            let mut names = vec![index.test_units[u].unit.item.clone()];
            while let Some(Some(p)) = closure.get(&u) {
                names.push(index.test_units[*p].unit.item.clone());
                u = *p;
            }
            names.reverse();
            names.join(" -> ")
        };

        let mut out = Vec::new();
        let row = |kind, unit: &Unit, hit: &TestHit, witness: String| Violation {
            kind,
            file: unit.file.clone(),
            item: unit.item.clone(),
            label: unit.label.clone(),
            subject: hit.subject.clone(),
            detail: hit.detail.clone(),
            witness,
        };
        for (u, tu) in index.test_units.iter().enumerate() {
            if tu.is_test {
                let closure = &closures[&u];
                for &v in closure.keys() {
                    for hit in &own[v] {
                        let witness = format!("{} -> {}", chain(closure, v), hit.subject);
                        out.push(row(hit.kind, &tu.unit, hit, witness));
                    }
                }
            } else {
                for hit in &own[u] {
                    let witness = format!("{} -> {}", tu.unit.item, hit.subject);
                    out.push(row(hit.kind, &tu.unit, hit, witness));
                }
            }
        }

        for use_item in &index.test_use_items {
            for entry in &use_item.entries {
                let mut kinds: BTreeMap<ViolationKind, String> = BTreeMap::new();
                let module = &use_item.module;
                let probe = match entry.kind {
                    UseKind::Name => {
                        for r in index
                            .resolve(&entry.segs, module, Scope::Tests)
                            .into_iter()
                            .flat_map(Target::into_refs)
                        {
                            if let Some(hit) = self.test_ref_hit(&r) {
                                kinds.entry(hit.kind).or_insert(hit.detail);
                            }
                        }
                        entry.segs.clone()
                    }
                    UseKind::Glob => {
                        if let Some(base) = index.abs_of(&entry.segs, module, 0, Scope::Tests) {
                            if let Some(hit) = self.test_ref_hit(&Ref::Module(base.clone())) {
                                kinds.entry(hit.kind).or_insert(hit.detail);
                            }
                            if state_modules.contains(&base) {
                                kinds
                                    .entry(ViolationKind::TestState)
                                    .or_insert_with(|| "globs a state module".into());
                            }
                        }
                        entry
                            .segs
                            .iter()
                            .cloned()
                            .chain(["_".to_string()])
                            .collect()
                    }
                };
                if let Some(kind) = index
                    .canonical(&probe, module, 0, Scope::Tests)
                    .and_then(|full| passage(&full, &state_modules))
                    .and_then(Self::test_passage_kind)
                {
                    kinds
                        .entry(kind)
                        .or_insert_with(|| "passes through it".into());
                }
                for (kind, detail) in kinds {
                    out.push(Violation {
                        kind,
                        file: use_item.file.clone(),
                        item: "<use>".into(),
                        label: "use".into(),
                        subject: entry.segs.join("::"),
                        detail,
                        witness: String::new(),
                    });
                }
            }
        }

        out.sort();
        out.dedup();
        let mut stale = Vec::new();
        for &(file, unit, kind, subject, reason) in exemptions {
            let before = out.len();
            out.retain(|v| {
                !(v.file == file && v.item == unit && v.kind == kind && v.subject == subject)
            });
            if out.len() == before {
                stale.push(format!("{file} `{unit}` {kind:?} {subject} ({reason})"));
            }
        }
        TestRegionCensus {
            violations: out,
            stale,
            parser_tests: tests.len(),
            benign_reads: benign,
        }
    }
}

#[test]
fn parser_test_regions_cross_no_future_crate_boundary() {
    let src = engine_src();
    let census = LayerCensus::run(
        &|rel: &str| std::fs::read_to_string(src.join(rel)).ok(),
        STATE_MODULE_ANCHORS,
        Overlay::Tests,
    );
    let regions = census.test_regions(TEST_REGION_EXEMPTIONS);

    let listed: Vec<String> = regions.violations.iter().map(ToString::to_string).collect();
    assert!(
        listed.is_empty(),
        "parser test code crosses a future crate boundary. Move the test to \
         `tests/integration/` (or to the game module it exercises), give it the parser or \
         AST-layer path of what it names, or add a `TEST_REGION_EXEMPTIONS` row with its \
         reason:\n{}",
        listed.join("\n")
    );
    assert!(
        regions.stale.is_empty(),
        "clause-6 exemption rows that match no hit; delete them: {:#?}",
        regions.stale
    );

    // Reach guards (floors per addenda/phase-5 §6.7): the overlay reads the test regions, and
    // the read scanner resolves both benign read classes.
    assert!(
        regions.parser_tests >= 11400,
        "reach-guard: parser tests {}",
        regions.parser_tests
    );
    for (file, unit, class) in [
        (
            "parser/oracle_condition.rs",
            "restriction_only_fallback_dispatches_exactly_the_pinned_families",
            ReadClass::InLayer,
        ),
        (
            "parser/oracle_keyword.rs",
            "keyword_payload_wire_golden_matches_the_client_fixture",
            ReadClass::OutOfCrate,
        ),
    ] {
        assert!(
            regions
                .benign_reads
                .iter()
                .any(|(f, u, c, _)| f == file && u == unit && *c == class),
            "reach-guard: `{file}:{unit}` makes a {class:?} read: {:#?}",
            regions.benign_reads
        );
    }
}

#[test]
fn the_test_region_census_flags_every_class() {
    let files: HashMap<&str, &str> = PLANTED_CRATE.iter().copied().collect();
    let census = LayerCensus::run(
        &|rel: &str| files.get(rel).map(|s| s.to_string()),
        &["types/game_state.rs", "types/state.rs"],
        Overlay::Tests,
    );
    let regions = census.test_regions(&[]);

    let touch = "game/obj.rs:touch";
    let touch_path = "crate::game::obj::touch";
    let obj = "game/obj.rs:Obj";
    let obj_path = "crate::game::obj::Obj";
    let ctl = "parser/ctl.rs";
    let mut expected: Vec<(ViolationKind, &str, &str, &str)> = vec![
        // A direct path, a test-module `use` binding (and the entry), a `super::*` glob, and a
        // helper closure (the helper's own row beside it).
        (ViolationKind::TestGameSide, ctl, "direct_path", touch),
        (ViolationKind::TestGameSide, ctl, "direct_path", touch_path),
        (ViolationKind::TestGameSide, ctl, "use_bound", obj),
        (ViolationKind::TestGameSide, ctl, "use_bound", obj_path),
        (ViolationKind::TestGameSide, ctl, "<use>", obj_path),
        (ViolationKind::TestGameSide, ctl, "through_glob", obj),
        (ViolationKind::TestGameSide, ctl, "through_helper", touch),
        (
            ViolationKind::TestGameSide,
            ctl,
            "through_helper",
            touch_path,
        ),
        (ViolationKind::TestGameSide, ctl, "reaches_game", touch),
        (ViolationKind::TestGameSide, ctl, "reaches_game", touch_path),
        // A trait and a glob reached only through a `use` entry.
        (
            ViolationKind::TestGameSide,
            ctl,
            "<use>",
            "crate::game::LocalExt",
        ),
        (
            ViolationKind::TestGameSide,
            ctl,
            "<use>",
            "crate::game::glob_mod",
        ),
        // A file module behind `#[cfg(test)] mod tests;`.
        (
            ViolationKind::TestGameSide,
            "parser/filemod/tests.rs",
            "in_file",
            touch,
        ),
        (
            ViolationKind::TestGameSide,
            "parser/filemod/tests.rs",
            "in_file",
            touch_path,
        ),
        // A `#[path]` file whose `use super::*` test reaches a library-level `#[cfg(test)]`
        // helper of the declaring file.
        (
            ViolationKind::TestGameSide,
            "parser/p.rs",
            "p_helper",
            touch,
        ),
        (
            ViolationKind::TestGameSide,
            "parser/p.rs",
            "p_helper",
            touch_path,
        ),
        (ViolationKind::TestGameSide, "parser/p_tests.rs", "t", touch),
        (
            ViolationKind::TestGameSide,
            "parser/p_tests.rs",
            "t",
            touch_path,
        ),
        // A library-level test-only parser fn.
        (
            ViolationKind::TestState,
            "parser/mod.rs",
            "test_only",
            "types/state.rs:Runtime",
        ),
        (
            ViolationKind::TestState,
            "parser/mod.rs",
            "test_only",
            "crate::types::state::Runtime",
        ),
        // A T item, and the crate-level loader and census.
        (
            ViolationKind::TestState,
            ctl,
            "state_item",
            "types/state.rs:Runtime",
        ),
        (
            ViolationKind::TestState,
            ctl,
            "state_item",
            "crate::types::state::Runtime",
        ),
        (
            ViolationKind::TestSupport,
            ctl,
            "names_test_support",
            "crate::test_support",
        ),
        (
            ViolationKind::TestSourceCensus,
            ctl,
            "names_source_census",
            "crate::source_census",
        ),
        // Cross-layer reads, one with no adjacent literal (fail closed).
        (
            ViolationKind::TestRead,
            ctl,
            "reads_ast_file",
            "src/types/x.rs",
        ),
        (
            ViolationKind::TestRead,
            ctl,
            "reads_manifest_game",
            "src/game/x.rs",
        ),
        (
            ViolationKind::TestRead,
            ctl,
            "reads_manifest_bare",
            "env!(\"CARGO_MANIFEST_DIR\")",
        ),
    ];
    expected.sort();
    let mut actual: Vec<(ViolationKind, &str, &str, &str)> = regions
        .violations
        .iter()
        .map(|v| (v.kind, v.file.as_str(), v.item.as_str(), v.subject.as_str()))
        .collect();
    actual.sort();
    actual.dedup();
    let listed: Vec<String> = regions.violations.iter().map(ToString::to_string).collect();
    assert_eq!(
        actual,
        expected,
        "planted clause-6 violations:\n{}",
        listed.join("\n")
    );
    for (file, item, chain) in [
        (
            ctl,
            "through_helper",
            "through_helper -> reaches_game -> game/obj.rs:touch",
        ),
        (
            "parser/p_tests.rs",
            "t",
            "t -> p_helper -> game/obj.rs:touch",
        ),
    ] {
        assert!(
            regions
                .violations
                .iter()
                .any(|v| v.file == file && v.item == item && v.witness == chain),
            "the witness for `{item}` reads `{chain}`:\n{}",
            listed.join("\n")
        );
    }

    let mut benign: Vec<(&str, ReadClass)> = regions
        .benign_reads
        .iter()
        .map(|(_, unit, class, _)| (unit.as_str(), *class))
        .collect();
    benign.sort();
    assert_eq!(
        benign,
        [
            ("reads_client", ReadClass::OutOfCrate),
            ("reads_self", ReadClass::InLayer)
        ],
        "a same-directory read and an out-of-crate read are not crossing"
    );

    // An exemption row removes its hit, and a row matching none is stale.
    let row = (
        ctl,
        "reads_ast_file",
        ViolationKind::TestRead,
        "src/types/x.rs",
        "planted",
    );
    let stale = (
        ctl,
        "reads_self",
        ViolationKind::TestRead,
        "ctl.rs",
        "planted",
    );
    let exempted = census.test_regions(&[row, stale]);
    assert_eq!(exempted.violations.len(), regions.violations.len() - 1);
    assert_eq!(exempted.stale.len(), 1, "{:#?}", exempted.stale);
    assert!(exempted.stale[0].contains("reads_self"));
}
