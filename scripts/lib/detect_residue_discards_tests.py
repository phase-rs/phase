#!/usr/bin/env python3
"""Gate H decisions: selected helper boundaries, not arbitrary residue dataflow."""
from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location(
    "residue", Path(__file__).with_name("detect-residue-discards.py"))
rd = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(rd)


def hits(src):
    return rd.find_discards("crates/engine/src/parser/probe.rs", src.splitlines())


class DiscardTests(unittest.TestCase):
    def test_helper_table_and_discard_bindings(self):
        for helper in rd.HELPERS:
            for rest in ("_", "_rem", "_rest"):
                syntax = ", syntax" if helper == "parse_target_with_syntax" else ""
                with self.subTest(helper=helper, rest=rest):
                    found = hits(f"let (value, {rest}{syntax}) = crate::target::{helper}(input, ctx);")
                    self.assertEqual([(h.family, h.symbol) for h in found], [("binding", helper)])

    def test_multiline_unbounded_and_wrapped_closures(self):
        for expr in (
            "parse_target_with_ctx(\n" + "input,\n" * 400 + "ctx)",
            "ctx.with_player_antecedents(|s| parse_target_with_ctx(input, s))",
            "ctx.with_player_antecedents(|s| { let x = input; parse_target_with_ctx(x, s) })",
            "ctx.with_scope(move |s| { parse_target_with_ctx(input, s) })",
            "(parse_target_with_ctx(input, ctx))",
        ):
            self.assertEqual(len(hits(f"let (value, _) = {expr};")), 1)

    def test_if_let_and_match(self):
        self.assertEqual(len(hits("if let Some((value, _)) = Some(parse_target(i)) { use_it(value); }")), 1)
        self.assertEqual(len(hits("match parse_target(i) { (value, _) => use_it(value), }")), 1)
        self.assertEqual(len(hits("match Some(parse_target(i)) { Some((value, _rest)) => use_it(value), None => (), }")), 1)

    def test_projections(self):
        for expr in ("parse_target(i).0", "(parse_target(i)).0", "parse_target_with_syntax(i, ctx).2"):
            self.assertEqual([h.family for h in hits(f"let value = {expr};")], ["projection"])
        self.assertEqual(hits("let rest = parse_target(i).1;"), [])

    def test_whole_result_drops(self):
        for expr in ("let _ = parse_target(i);", "drop(parse_target(i));", "parse_target(i);",
                     "std::mem::drop((parse_target(i)));", "let _ = (parse_target(i));",
                     "(parse_target(i));", "ctx.with_scope(|s| parse_target(i, s));"):
            self.assertEqual([h.family for h in hits(expr)], ["whole-result"])

    def test_alias_imports(self):
        for imp in ("use crate::target::parse_target as read;", "use crate::target::{parse_target as read, Other};"):
            self.assertEqual([h.symbol for h in hits(imp + "\nlet (v, _) = read(i);")], ["parse_target"])

    def test_exact_defaults_independent_of_producer(self):
        for variant in ("SelfRef", "Any"):
            self.assertEqual([h.family for h in hits(f"let f = producer().unwrap_or(TargetFilter::{variant});")], ["default"])
        self.assertEqual(hits("let f = producer().unwrap_or_else(|| TargetFilter::Any);"), [])
        self.assertEqual(hits("let f = producer().unwrap_or(Other::Any);"), [])

    def test_nine_default_risk_sites_are_counted_without_producer_assumptions(self):
        producers = ("hand().map(|(_, t)| t)",) * 3 + (
            "scan()", "prevention().filter(|t| valid(t))", "def.affected.take()",
            "subject()", "attached().map(|(f, _)| f)", "filter.clone()")
        variants = ("Any", "Any", "Any", "SelfRef", "Any", "Any", "SelfRef", "SelfRef", "Any")
        found = hits("\n".join(f"let f = {p}.unwrap_or(TargetFilter::{v});"
                               for p, v in zip(producers, variants)))
        self.assertEqual([h.family for h in found], ["default"] * 9)

    def test_consumed_results_are_not_ignored_statements(self):
        for src in ("fn f() { return parse_target(i); }",
                    "fn f() { parse_target(i) }",
                    "let result = parse_target(i);",
                    "let (v, rest) = ctx.with_scope(|s| { parse_target(i) });"):
            self.assertEqual(hits(src), [])
        self.assertEqual([h.family for h in hits("fn f() { return parse_target(i).0; }")],
                         ["projection"])

    def test_each_match_arm_discard_is_a_distinct_syntactic_occurrence(self):
        self.assertEqual(len(hits("match parse_target(i) { (Typed(t), _) => t, (other, _) => other, }")), 2)

    def test_nom_unit_outputs_with_production_witness(self):
        for helper in ("parse_target_demonstrative_subject", "parse_target_anaphoric_subject"):
            src = (f"fn {helper}(i: &str) -> OracleResult<'_, ()> {{ Ok((i, ())) }}\n"
                   f"let (rest, _) = {helper}(input)?;\n"
                   "let (value, _) = parse_target(input);")
            self.assertEqual([(h.line, h.family) for h in hits(src)], [(3, "binding")])

    def test_nested_consumed_helper_not_outer_tuple_result(self):
        for src in (
            "let (v, _) = consume(parse_target(i));",
            "let (v, _) = ctx.with_player_antecedents(|s| consume(parse_target(i)));",
            "let (v, rest) = parse_target(i); consume(rest);",
            "let (rest, _) = parse_target_demonstrative_subject(i)?;",
            "let (rest, _) = parse_target_anaphoric_subject(i)?;",
        ):
            self.assertEqual(hits(src), [])

    def test_lexical_and_cfg_exclusions_have_production_witness(self):
        excluded = [
            '// let (v, _) = parse_target(i);',
            '/* nested /* comment */ let (v, _) = parse_target(i); */',
            'let text = "let (v, _) = parse_target(i);";',
            'let text = r###"let (v, _) = parse_target(i); {"###;',
            'let text = br#"let (v, _) = parse_target(i);"#;',
            "let brace = '}';",
            '#[cfg(test)]\nfn hidden() { let (v, _) = parse_target(i); }',
            '#[cfg(test)]\n#[allow(dead_code)]\nmod hidden { fn f() { parse_target(i); } }',
        ]
        for src in excluded:
            with self.subTest(src=src):
                found = hits(src + "\nfn witness<'a>(i: &'a str) { let (v, _) = parse_target(i); }")
                self.assertEqual([(h.line, h.family) for h in found], [(len(src.splitlines()) + 1, "binding")])

    def test_discovery_excludes_test_files_not_production_witness(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            parser = root / rd.SCOPE
            parser.mkdir(parents=True)
            files = {
                "mod.rs": '#[cfg(test)]\nmod support;\n#[cfg(test)]\n#[path = "odd.rs"]\nmod custom;\n#[cfg(test)]\nmod fixtures;\n#[cfg(any(test, unix))]\nmod retained;\n',
                "support.rs": "let (v, _) = parse_target(i);",
                "odd.rs": "let (v, _) = parse_target(i);",
                "fixtures/mod.rs": "mod child;",
                "fixtures/child.rs": "let (v, _) = parse_target(i);",
                "test_support.rs": "#![cfg(test)]\nlet (v, _) = parse_target(i);",
                "unit_tests.rs": "let (v, _) = parse_target(i);",
                "snapshots/fake.rs": "let (v, _) = parse_target(i);",
                "retained.rs": "let (v, _) = parse_target(i);",
                "production.rs": "let (v, _) = parse_target(i);",
            }
            for name, text in files.items():
                path = parser / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            sources = rd.production_sources(root, False)
            found = [h for path, text in sources.items() for h in rd.find_discards(path, text.splitlines())]
            self.assertEqual(sorted(Path(h.path).name for h in found), ["production.rs", "retained.rs"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
