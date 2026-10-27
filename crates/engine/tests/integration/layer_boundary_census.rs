//! Layer-boundary census for the crate split: parser library code names no game-side module.
//!
//! Until the parser is its own crate, nothing but this census stops a parser edit from
//! reaching back into `game` (or any other game-side module) and silently re-coupling the
//! two. The census parses the source with `syn` instead of grepping it, so comments and
//! doc text cannot count as references.
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
//!   neither the crate root nor the AST layer may re-export a game-side module, or a parser
//!   path through them would cross the boundary without naming it.
//!
//! Fail-closed limits: a `#[cfg(test)]` on a statement inside a library function is not
//! honoured (the statement is scanned as library code), and a `#[path]` inside an inline
//! module panics instead of being guessed at.

use std::collections::BTreeSet;
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
    macros: Vec<String>,
    hits: Vec<Hit>,
    files: Vec<(PathBuf, Reach)>,
}

impl Walk {
    fn new(src: &Path, scan: Scan, macros: Vec<String>) -> Self {
        Self {
            src: src.to_path_buf(),
            scan,
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
                        let target = match path_attr(&m.attrs) {
                            Some(p) => {
                                assert!(!inline, "#[path] inside an inline module in {}", ctx.rel);
                                ctx.dir.join(p)
                            }
                            None => {
                                let flat = child_dir.join(format!("{}.rs", m.ident));
                                if flat.exists() {
                                    flat
                                } else {
                                    child_dir.join(m.ident.to_string()).join("mod.rs")
                                }
                            }
                        };
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
        if head_is_root_or_bare && path.get(1).is_some_and(|s| GAME_SIDE.contains(&s.as_str())) {
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
                        for segment in root_segments(toks, after) {
                            if GAME_SIDE.contains(&segment) {
                                self.hit(file, module, format!("{t}-headed path to {segment}"));
                            }
                        }
                    }
                    Some(_) => {}
                    None => {
                        if GAME_SIDE.contains(&t) && toks.get(i + 1).is_some_and(|n| n == "::") {
                            self.hit(file, module, format!("bare-headed path to {t}"));
                        }
                    }
                }
            }
            if let Some(lit) = t.strip_prefix("<lit>") {
                let names_game_side = syn::parse_str::<syn::LitStr>(lit).is_ok_and(|s| {
                    let value = s.value();
                    GAME_SIDE.iter().any(|g| {
                        value
                            .strip_prefix("crate::")
                            .and_then(|rest| rest.strip_prefix(g))
                            .is_some_and(|rest| rest.starts_with("::"))
                    })
                });
                if names_game_side {
                    self.hit(file, module, format!("path string {lit}"));
                }
            }
            if self.macros.iter().any(|m| m == t) && toks.get(i + 1).is_some_and(|n| n == "!") {
                self.hit(file, module, format!("game-side macro {t}!"));
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
/// game-side module for a `*` glob of the crate root.
fn root_segments(toks: &[String], after: usize) -> Vec<&str> {
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
        Some("*") => GAME_SIDE.to_vec(),
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

/// `#[macro_export]` macros defined in game-side modules.
fn game_side_exported_macros(src: &Path) -> Vec<String> {
    fn collect(items: &[syn::Item], out: &mut Vec<String>) {
        for item in items {
            match item {
                syn::Item::Macro(m)
                    if m.attrs.iter().any(|a| a.path().is_ident("macro_export")) =>
                {
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
    for g in GAME_SIDE {
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
    scan_source_with(src, module, Scan::Library)
}

fn scan_source_with(src: &str, module: &[&str], scan: Scan) -> Vec<Hit> {
    let parsed = syn::parse_file(src).expect("planted source parses");
    let mut walk = Walk::new(Path::new(""), scan, vec!["assert_engine_eq".to_string()]);
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
    let macros = game_side_exported_macros(&src);
    assert!(
        macros.iter().any(|m| m == "assert_engine_eq"),
        "reach-guard: the exported game-side macro set lost `assert_engine_eq`: {macros:?}"
    );
    let mut walk = Walk::new(&src, Scan::Library, macros);
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
         into the AST layer (`types/`) and re-point the parser call, or keep the reference in \
         test code: {hits:#?}"
    );
}

#[test]
fn no_root_or_types_reexport_reaches_game_side() {
    let src = engine_src();
    let mut root = Walk::new(&src, Scan::Reexports(UseVisibility::Any), Vec::new());
    root.walk_file(
        &src.join("lib.rs"),
        Vec::new(),
        Reach::Library,
        Children::Skip,
    );
    assert!(
        root.hits.is_empty(),
        "`lib.rs` imports a game-side module through a `use`, so a crate-root path could \
         reach it from the parser: {:#?}",
        root.hits
    );

    let mut types = Walk::new(&src, Scan::Reexports(UseVisibility::NonPrivate), Vec::new());
    types.walk_file(
        &src.join("types/mod.rs"),
        vec!["types".to_string()],
        Reach::Library,
        Children::Follow,
    );
    let reached = types.files_reached(Reach::Library);
    assert!(
        reached.len() >= 30 && reached.contains("types/ability.rs"),
        "reach-guard: the types walk reached {} library files: {reached:?}",
        reached.len()
    );
    assert!(
        types.hits.is_empty(),
        "the AST layer re-exports a game-side module, so a parser path through `types` would \
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

    // Clause 2 visibility: a private `use` in the AST layer is not a re-export.
    let reexports = |src: &str| {
        scan_source_with(
            src,
            &["types", "x"],
            Scan::Reexports(UseVisibility::NonPrivate),
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
