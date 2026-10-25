#!/usr/bin/env python3
"""Gate H decisions: selected helper boundaries, not arbitrary residue dataflow."""
from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from contextlib import redirect_stderr
from io import StringIO
from unittest.mock import patch

_SPEC = importlib.util.spec_from_file_location(
    "residue", Path(__file__).with_name("detect-residue-discards.py"))
rd = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(rd)


def hits(src):
    return rd.find_discards("crates/engine/src/parser/probe.rs", src.splitlines())


class DiscardTests(unittest.TestCase):
    def test_o_refutable_let_else_and_if_let_else(self):
        self.assertEqual([h.family for h in hits(
            "fn probe(input: &str) -> Option<TargetFilter> { "
            "let (TargetFilter::Typed(typed), _) = parse_target(input) "
            "else { return None; }; Some(TargetFilter::Typed(typed)) }")], ["binding"])
        for expr in (
            "parse_target(input)",
            "ctx.with_player_antecedents(|s| parse_target(input))",
            "ctx.with_player_scope(scope, |s| { parse_target(input) })",
        ):
            for annotation in ("", ": (TargetFilter, &str)"):
                for modifier in ("", "mut ", "ref ", "ref mut "):
                    for residue, expected in (("_rest", ["binding"]), ("rest", [])):
                        pattern = f"(TargetFilter::Typed(typed), {modifier}{residue})"
                        src = (f"fn probe(input: &str) -> Option<TargetFilter> {{ "
                               f"let {pattern}{annotation} = {expr} else {{ return None; }}; "
                               f"consume({residue}); Some(TargetFilter::Typed(typed)) }}")
                        with self.subTest(expr=expr, annotation=annotation,
                                          modifier=modifier, residue=residue):
                            self.assertEqual([h.family for h in hits(src)], expected)
            for residue, expected in (("_rest", ["binding"]), ("rest", [])):
                src = (f"if let (TargetFilter::Typed(typed), {residue}) = {expr} "
                       "{ consume(" + residue + "); } else { return None; }")
                with self.subTest(expr=expr, residue=residue):
                    self.assertEqual([h.family for h in hits(src)], expected)

    def test_p_or_patterns_count_each_discarding_alternative(self):
        for pattern in (
            "(TargetFilter::Any, REST) | (TargetFilter::SelfRef, REST)",
            "| (TargetFilter::Any, REST) | (TargetFilter::SelfRef, REST)",
            "((TargetFilter::Any, REST) | (TargetFilter::SelfRef, REST))",
            "((TargetFilter::Any, REST)) | ((TargetFilter::SelfRef, REST))",
        ):
            for residue, expected in (("_", 2), ("rest", 0)):
                body = "None" if residue == "_" else "consume(rest)"
                src = ("match parse_target(input) { " + pattern.replace("REST", residue)
                       + " => " + body + ", "
                       "(filter, rest) => rest.is_empty().then_some(filter), }")
                with self.subTest(pattern=pattern, residue=residue):
                    self.assertEqual([h.family for h in hits(src)], ["binding"] * expected)
        for pattern in (
            "(TargetFilter::Any, _) | (TargetFilter::SelfRef, rest)",
            "(TargetFilter::Any, rest) | (TargetFilter::SelfRef, _)",
        ):
            with self.subTest(mixed=pattern):
                self.assertEqual([h.family for h in hits(
                    f"match parse_target(input) {{ {pattern} => consume(rest), }}")],
                    ["binding"])
        combined = ("match parse_target(input) { "
                    "(TargetFilter::Any, _) | (TargetFilter::SelfRef, _) => None, }")
        split = combined.replace(" | ", " => None, ")
        self.assertEqual(len(hits(combined)), 2)
        self.assertEqual(len(hits(split)), 2)

    def test_supported_binding_matrix_discarded_vs_consumed(self):
        # Deterministic cross-product, not randomized fuzzing: each grammar axis
        # carries a consumed-remainder control, including inside both wrappers.
        forms = (
            ("plain", "let (filter, REST) = EXPR; consume(REST);", 1),
            ("typed", "let (filter, REST): (TargetFilter, &str) = EXPR; consume(REST);", 1),
            ("mut", "let (filter, mut REST) = EXPR; consume(REST);", 1),
            ("ref", "let (filter, ref REST) = EXPR; consume(REST);", 1),
            ("ref-mut", "let (filter, ref mut REST) = EXPR; consume(REST);", 1),
            ("tuple-rest", "let (filter, REST, ..) = EXPR; consume(REST);", 1),
            ("let-else", "let (Typed(filter), REST) = EXPR else { return; }; consume(REST);", 1),
            ("if-let", "if let (Typed(filter), REST) = EXPR { consume(REST); } else {}", 1),
            ("match", "match EXPR { (Typed(filter), REST) => consume(REST), }", 1),
            ("or-pattern", "match EXPR { (Any, REST) | (SelfRef, REST) => consume(REST), }", 2),
        )
        for helper in ("parse_target", "parse_target_with_syntax"):
            call = f"{helper}(input)"
            for expr in (call, f"ctx.with_player_antecedents(|s| {call})",
                         f"ctx.with_player_scope(scope, |s| {{ {call} }})"):
                for name, template, discards in forms:
                    for residue in ("_rest", "rest"):
                        src = template.replace("REST", residue).replace("EXPR", expr)
                        if helper == "parse_target_with_syntax":
                            src = src.replace(", " + residue + ")", ", " + residue + ", syntax)")
                            src = src.replace("(TargetFilter, &str)", "(TargetFilter, &str, Syntax)")
                        elif name == "tuple-rest":
                            pattern = "(filter, ..)" if residue == "_rest" else "(.., rest)"
                            src = src.replace(f"(filter, {residue}, ..)", pattern)
                        with self.subTest(helper=helper, expr=expr, form=name, residue=residue):
                            self.assertEqual([h.family for h in hits(src)],
                                             ["binding"] * (discards if residue == "_rest" else 0))

    def test_h_block_closure_tail_after_expression_statements(self):
        for statement in ("if input.is_empty() { observe(); }",
                          "if flag { observe(); } else { observe(); }",
                          "match flag { true => {}, false => {} }",
                          "unsafe { observe(); }", "loop { break; }",
                          "while flag { break; }", "for x in xs { observe(); }"):
            expr = ("ctx.with_player_antecedents(|s| { " + statement
                    + " parse_target_with_ctx(input, s) })")
            with self.subTest(statement=statement):
                self.assertEqual([h.family for h in hits(f"let (v, _) = {expr};")], ["binding"])
                self.assertEqual(hits(f"let (v, rest) = {expr}; consume(rest);"), [])

    def test_i_comma_free_expression_with_block_match_arms(self):
        for body in ("if rest.is_empty() { observe(); }",
                     "if flag { consume(rest); } else { consume(rest); }",
                     "match flag { true => consume(rest), false => () }",
                     "unsafe { consume(rest); }", "loop { consume(rest); break; }",
                     "while flag { consume(rest); break; }",
                     "for x in xs { consume(rest); }"):
            with self.subTest(body=body):
                prefix = "match parse_target(input) { (TargetFilter::Any, rest) => " + body
                self.assertEqual([h.family for h in hits(prefix + " (v, _) => use_it(v), }")],
                                 ["binding"])
                self.assertEqual(hits(prefix + " (v, rest) => consume(rest), }"), [])

    def test_j_aliases_are_scoped_to_lexical_modules(self):
        src = ("mod a { use crate::target::parse_target_with_syntax as read; "
               "fn f(i: &str) { let s = read(i, ctx).2; } }\n"
               "mod b { use crate::target::parse_target as read; "
               "fn g(i: &str) { let (v, rest) = read(i); consume(rest); } }\n"
               "mod c { fn read(i: &str) -> (u8, u8) { (0, 0) } "
               "fn h(i: &str) { let (v, _) = read(i); } }")
        self.assertEqual([(h.line, h.family, h.symbol) for h in hits(src)],
                         [(1, "projection", "parse_target_with_syntax")])
        self.assertEqual(hits(src.replace("read(i, ctx).2", "read(i, ctx).1")), [])

    def test_k_binding_modifiers_preserve_ignored_remainder(self):
        for modifier in ("mut", "ref", "ref mut"):
            with self.subTest(modifier=modifier):
                self.assertEqual([h.family for h in hits(
                    f"let (value, {modifier} _rest) = parse_target(i);")], ["binding"])
                self.assertEqual(hits(
                    f"let (value, {modifier} rest) = parse_target(i); consume(rest);"), [])

    def test_l_typed_wildcard_drops_whole_result(self):
        self.assertEqual([h.family for h in hits(
            "let _: (TargetFilter, &str) = parse_target(input);")], ["whole-result"])
        self.assertEqual(hits(
            "let result: (TargetFilter, &str) = parse_target(input); consume(result.1);"), [])

    def test_m_uncalled_closure_is_not_a_helper_result(self):
        for closure in ("|| parse_target(input)", "move || parse_target(input)",
                        "|s| { parse_target_with_ctx(input, s) }"):
            with self.subTest(closure=closure):
                src = f"let _ = {closure};\nlet (v, _) = parse_target(witness);"
                self.assertEqual([(h.line, h.family) for h in hits(src)], [(2, "binding")])

    def test_n_precise_capture_is_not_an_import(self):
        capture = "fn keep<'a>(i: &'a str) -> impl Fn() -> &'a str + use<'a> { move || i }"
        self.assertEqual(hits(capture), [])
        self.assertEqual([h.family for h in hits(
            capture + "\nfn f() { let (v, _) = parse_target(input); }")], ["binding"])

    def test_n_precise_capture_module_discovery(self):
        capture = "fn keep<'a>(i: &'a str) -> impl Fn() -> &'a str + use<'a> { move || i }"
        for source in (capture + "\nmod child;", "mod child;\n" + capture):
            with self.subTest(source=source):
                self.assertEqual(self.discovery_hits({
                    "mod.rs": source,
                    "child.rs": "fn f() { let (v, _) = parse_target(input); }",
                }), [("child.rs", "binding")])

    def test_n_import_item_visibilities(self):
        for visibility in ("", "pub ", "pub(crate) "):
            self.assertEqual([h.symbol for h in hits(
                f"{visibility}use crate::target::parse_target as read;\n"
                "fn f() { let (v, _) = read(input); }")], ["parse_target"])

    def test_n_internal_errors_name_file_without_traceback(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            parser = root / rd.SCOPE
            parser.mkdir(parents=True)
            (parser / "probe.rs").write_text("fn f() {}")
            for operation in ("module_edges", "find_discards"):
                stderr = StringIO()
                with self.subTest(operation=operation):
                    with patch.object(rd, operation, side_effect=RuntimeError("injected failure")), \
                            patch("sys.argv", ["detector", "--list", "--root", tmp]), \
                            redirect_stderr(stderr):
                        self.assertEqual(rd.main(), 1)
                    self.assertIn("detector internal error at crates/engine/src/parser/probe.rs",
                                  stderr.getvalue())
                    self.assertIn("RuntimeError: injected failure", stderr.getvalue())
                    self.assertNotIn("Traceback", stderr.getvalue())

    def test_helper_table_and_discard_bindings(self):
        for helper in rd.HELPERS:
            for rest in ("_", "_rem", "_rest"):
                syntax = ", syntax" if helper == "parse_target_with_syntax" else ""
                with self.subTest(helper=helper, rest=rest):
                    found = hits(f"let (value, {rest}{syntax}) = crate::target::{helper}(input, ctx);")
                    self.assertEqual([(h.family, h.symbol) for h in found], [("binding", helper)])

    def test_unknown_closure_consumers_do_not_forward_helper_results(self):
        for consumer in ("into_nom(|| parse_target(input))",
                         "ctx.into_nom(|s| parse_target(input))"):
            src = ("fn into_nom(f: impl FnOnce() -> (TargetFilter, &str)) -> (&str, ()) "
                   "{ let (_, rest) = f(); (rest, ()) }\n"
                   f"let (rest, _) = {consumer};\n"
                   "let (value, _) = parse_target(witness);")
            self.assertEqual([(h.line, h.family) for h in hits(src)], [(3, "binding")])

    def test_established_scope_wrapper_forwards_last_closure_result(self):
        src = "let (value, _) = ctx.with_player_scope(scope, |s| parse_target_with_ctx(input, s));"
        self.assertEqual([(h.family, h.symbol) for h in hits(src)],
                         [("binding", "parse_target_with_ctx")])

    def test_typed_binding_discards_residue(self):
        found = hits("let (filter, _): (TargetFilter, &str) = parse_target(input);")
        self.assertEqual([(h.family, h.symbol) for h in found], [("binding", "parse_target")])
        self.assertEqual(hits("let (filter, rest): (TargetFilter, &str) = parse_target(input); consume(rest);"), [])

    def test_tuple_rest_patterns_discard_only_when_covering_residue(self):
        for pattern, helper in (("(filter, ..)", "parse_target"),
                                ("(..)", "parse_target"),
                                ("(filter, ..)", "parse_target_with_syntax"),
                                ("(.., syntax)", "parse_target_with_syntax")):
            with self.subTest(pattern=pattern, helper=helper):
                self.assertEqual([h.family for h in hits(f"let {pattern} = {helper}(input);")], ["binding"])
        for pattern, helper in (("(.., rest)", "parse_target"),
                                ("(filter, rest, ..)", "parse_target_with_syntax")):
            with self.subTest(pattern=pattern, helper=helper):
                self.assertEqual(hits(f"let {pattern} = {helper}(input); consume(rest);"), [])

    def test_multiline_unbounded_and_wrapped_closures(self):
        for expr in (
            "parse_target_with_ctx(\n" + "input,\n" * 400 + "ctx)",
            "ctx.with_player_antecedents(|s| parse_target_with_ctx(input, s))",
            "ctx.with_player_antecedents(|s| { let x = input; parse_target_with_ctx(x, s) })",
            "ctx.with_player_scope(scope, move |s| { parse_target_with_ctx(input, s) })",
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
                     "(parse_target(i));", "ctx.with_player_antecedents(|s| parse_target(i, s));"):
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
                    "let (v, rest) = ctx.with_player_antecedents(|s| { parse_target(i) });"):
            self.assertEqual(hits(src), [])
        self.assertEqual([h.family for h in hits("fn f() { return parse_target(i).0; }")],
                         ["projection"])

    def test_guarded_match_arm_discards_are_counted(self):
        src = ("match parse_target(i) { (v, _) if keep(&v) => use_it(v), "
               "(v, rest) => consume(rest), }")
        self.assertEqual([h.family for h in hits(src)], ["binding"])

    def test_block_match_arms_need_no_trailing_comma(self):
        for src, expected in (
            ("match parse_target(i) { (Typed(v), rest) => { consume(rest); } "
             "(v, _) => { use_it(v); } }", 1),
            ("match parse_target(i) { (Typed(v), _) => { use_it(v); } "
             "(v, _) if keep(&v) => { use_it(v); } }", 2),
        ):
            with self.subTest(src=src):
                self.assertEqual([h.family for h in hits(src)], ["binding"] * expected)

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

    def test_attributed_non_items_preserve_gate_witness(self):
        for container in (
            "struct S { #[cfg(all(test, unix))] hidden: (), }",
            "enum E { #[cfg(all(test, unix))] Hidden, Visible }",
            "fn f() { match x { #[cfg(all(test, unix))] Hidden => (), Visible => () } }",
            "fn f() { #[cfg(all(test, unix))] let hidden = (); }",
        ):
            with self.subTest(container=container):
                found = hits(container + "\nfn witness() { let (v, _) = parse_target(i); }")
                self.assertEqual([(h.line, h.family) for h in found], [(2, "binding")])

    def test_const_generic_test_impl_excludes_helper_discards(self):
        src = ("#[cfg(test)] impl Foo<{ 1 }> { fn hidden(i: &str) { let (v, _) = parse_target(i); } }\n"
               "fn witness(i: &str) { let (v, _) = parse_target(i); }")
        self.assertEqual([(h.line, h.family) for h in hits(src)], [(2, "binding")])

    def discovery_hits(self, files):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name, text in files.items():
                path = root / rd.SCOPE / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            return [(Path(h.path).relative_to(rd.SCOPE).as_posix(), h.family)
                    for path, text in rd.production_sources(root, False).items()
                    for h in rd.find_discards(path, text.splitlines())]

    def test_production_module_reference_wins_over_test_path_alias(self):
        found = self.discovery_hits({
            "mod.rs": 'mod x;\n#[cfg(test)]\n#[path = "x.rs"]\nmod y;\nmod witness;',
            "x.rs": "let (v, _) = parse_target(input);",
            "witness.rs": "let (v, _) = parse_target(input);",
        })
        self.assertEqual(sorted(found), [("witness.rs", "binding"), ("x.rs", "binding")])

    def test_outlined_test_child_uses_enclosing_inline_module_directory(self):
        found = self.discovery_hits({
            "mod.rs": "mod outer { #[cfg(test)] mod support; }\nmod witness;",
            "outer/support.rs": "let (v, _) = parse_target(hidden);",
            "support.rs": "let (v, _) = parse_target(production);",
            "witness.rs": "let (v, _) = parse_target(production);",
        })
        self.assertEqual(sorted(found), [("support.rs", "binding"), ("witness.rs", "binding")])

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
