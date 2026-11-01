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
    return rd.find_discards("crates/phase-parser/src/parser/probe.rs", src.splitlines())


class DiscardTests(unittest.TestCase):
    def test_s_named_whole_result_bindings(self):
        forms = (
            "let PATTERN = EXPR; consume(NAME);",
            "let PATTERN: (TargetFilter, &str) = EXPR; consume(NAME);",
            "let PATTERN = EXPR else { return; }; consume(NAME);",
            "if let PATTERN = EXPR { consume(NAME); } else {}",
        )
        for modifier in ("", "ref ", "mut ", "ref mut "):
            for template in forms:
                for expr in (
                    "parse_target(i)",
                    "parse_target(\n    i,\n)",
                    "ctx.with_player_antecedents(|s| parse_target(i, s))",
                    "ctx.with_player_scope(scope, |s| { parse_target(i, s) })",
                ):
                    for name, expected in (("_r", ["whole-result"]), ("r", [])):
                        source = template.replace("PATTERN", modifier + name).replace(
                            "EXPR", expr).replace("NAME", name)
                        with self.subTest(modifier=modifier, template=template,
                                          expr=expr, name=name):
                            self.assertEqual([h.family for h in hits(source)], expected)
                    for expr_control in (
                        f"consume({expr})", f"|s| {expr}", f"move |s| {{ {expr} }}",
                    ):
                        source = template.replace("PATTERN", modifier + "_r").replace(
                            "EXPR", expr_control).replace("NAME", "_r")
                        with self.subTest(control=source):
                            self.assertEqual(hits(source), [])

    def test_s_whole_result_match_alternatives(self):
        patterns = (
            ("NAME", 1),
            ("ref NAME", 1),
            ("mut NAME", 1),
            ("ref mut NAME", 1),
            ("NAME | NAME", 2),
            ("| NAME | NAME", 2),
            ("(NAME | NAME)", 2),
            ("(NAME) | (NAME)", 2),
        )
        for pattern, count in patterns:
            for guard in ("", " if keep(&NAME)"):
                for name in ("_", "_unused", "result"):
                    source = ("match parse_target(\n    i,\n) {\n    "
                              + pattern + guard + " => { consume(NAME); },\n}")
                    source = source.replace("NAME", name)
                    with self.subTest(pattern=pattern, guard=guard, name=name):
                        self.assertEqual([h.family for h in hits(source)],
                                         ["whole-result"] * (0 if name == "result" else count))
                    self.assertEqual(hits(source.replace(
                        "parse_target(\n    i,\n)", "consume(parse_target(i))")), [])
        for pattern in ("_unused | (v, _rest)", "(v, _rest) | _unused"):
            with self.subTest(mixed=pattern):
                self.assertCountEqual([h.family for h in hits(
                    f"match parse_target(i) {{ {pattern} if keep() => {{}}, }}")],
                    ["whole-result", "binding"])

    def test_drop_turbofish_preserves_whole_result_classification(self):
        for drop in ("drop", "std::mem::drop", "mem::drop"):
            for generic in ("", "::<_>", "::<(T, &str)>"):
                for comma in ("", ","):
                    call = drop + generic
                    with self.subTest(call=call, comma=comma):
                        self.assertEqual([(h.family, h.symbol) for h in hits(
                            f"fn f(i: &str) {{ {call}(parse_target(i){comma}); }}")],
                            [("whole-result", "parse_target")])
                        for arg in ("consume(parse_target(i))", "unrelated(i)",
                                    "parse_target(i), unrelated(i)"):
                            self.assertEqual(hits(
                                f"fn f(i: &str) {{ {call}({arg}{comma}); }}"), [])
        for source in ("let value = object.drop;", "let value = mem::drop;",
                       "let value = std::mem::drop::<_>;",
                       "object.drop::<_>(parse_target(i));",
                       "(object).drop::<(T, &str)>(parse_target(i));",
                       "object.drop(consume(parse_target(i)));",
                       "object.drop::<_>(consume(parse_target(i)));"):
            with self.subTest(source=source):
                self.assertEqual(hits(source), [])

    def test_drop_methods_do_not_discard_helper_arguments(self):
        for receiver in ("reader", "(reader)", "factory()", "readers[0]"):
            for generic in ("", "::<_>", "::<(T, &str)>"):
                for arg in ("parse_target(i)", "consume(parse_target(i))"):
                    source = f"{receiver}.drop{generic}({arg});"
                    with self.subTest(source=source):
                        self.assertEqual(hits(source), [])
        for prefix in ("..", "a.."):
            for generic in ("", "::<_>", "::<(T, &str)>"):
                source = f"let range = {prefix}drop{generic}(parse_target(i));"
                with self.subTest(source=source):
                    self.assertEqual([(h.family, h.symbol) for h in hits(source)],
                                     [("whole-result", "parse_target")])

    def test_bare_block_statements_discard_only_helper_tails(self):
        for helper in ("parse_target", "parse_target_with_syntax"):
            for template in (
                "EXPR;",
                "fn f(i: &str) { EXPR; }",
                "fn f(i: &str) { observe(); EXPR; }",
                "fn f(i: &str) { { observe(); } EXPR; }",
            ):
                expr = f"{{ let t = i.trim(); {helper}(t) }}"
                for tail, expected in (
                    (f"{helper}(t)", [("whole-result", helper)]),
                    (f"consume({helper}(t))", []),
                    (f"{helper}(t).1", []),
                    ("unrelated(t)", []),
                ):
                    source = template.replace("EXPR", expr.replace(f"{helper}(t)", tail))
                    with self.subTest(source=source):
                        self.assertEqual([(h.family, h.symbol) for h in hits(source)],
                                         expected)

    def test_block_headers_preserve_existing_classification(self):
        for source in (
            "if c { parse_target(i) };",
            "if c {} else { parse_target(i) };",
            "match x { _ => parse_target(i) };",
            "loop { parse_target(i) };",
            "unsafe { parse_target(i) };",
            "fn f() { parse_target(i) };",
            "while ready() { parse_target(i) };",
            "let result = { parse_target(i) };",
        ):
            with self.subTest(source=source):
                self.assertEqual(hits(source), [])

    def test_block_statement_recognizers_do_not_double_count(self):
        for helper in ("parse_target", "parse_target_with_syntax"):
            for source, family in (
                (f"let _ = {{ {helper}(i) }};", "whole-result"),
                (f"drop({{ {helper}(i) }});", "whole-result"),
                (f"{{ drop({helper}(i)) }};", "whole-result"),
                (f"{{ let t = i.trim(); {helper}(t) }}.0;", "projection"),
                (f"{{ {helper}(i).0 }};", "projection"),
            ):
                with self.subTest(source=source):
                    self.assertEqual([(h.family, h.symbol) for h in hits(source)],
                                     [(family, helper)])

    def test_q_trailing_comma_drop_arguments(self):
        for drop in ("drop", "std::mem::drop"):
            with self.subTest(drop=drop):
                self.assertEqual([h.family for h in hits(
                    f"{drop}(\n    parse_target(\n        i,\n    ),\n);")],
                    ["whole-result"])
                self.assertEqual(hits(
                    f"{drop}(\n    consume(parse_target(i)),\n);"), [])
                self.assertEqual(hits(
                    f"{drop}(\n    unrelated(i),\n);"), [])
                self.assertEqual(hits(
                    f"{drop}(parse_target(i), unrelated(i));"), [])

    def test_q_trailing_comma_exact_defaults_in_nested_expressions(self):
        for variant in ("Any", "SelfRef"):
            for template in ("EXPR", "consume(Some(\n    EXPR,\n))",
                             "let f = (EXPR, unrelated());"):
                with self.subTest(variant=variant, template=template):
                    expr = f"x\n    .unwrap_or(\n        TargetFilter::{variant},\n    )"
                    self.assertEqual([h.family for h in hits(template.replace("EXPR", expr))],
                                     ["default"])
                    for control in (f"x.unwrap_or(\n    Other::{variant},\n)",
                                    f"x.unwrap_or(\n    make(TargetFilter::{variant}),\n)",
                                    f"x.unwrap_or(TargetFilter::{variant}, unrelated())"):
                        self.assertEqual(hits(template.replace("EXPR", control)), [])

    def test_q_rustfmt_binding_matrix(self):
        forms = (
            "let (filter, REST,) = EXPR; consume(REST);",
            "let (filter, REST,): (TargetFilter, &str) = EXPR; consume(REST);",
            "let (filter, mut REST,) = EXPR; consume(REST);",
            "let (filter, ref REST,) = EXPR; consume(REST);",
            "let (filter, ref mut REST,) = EXPR; consume(REST);",
            "let (filter, REST, ..) = EXPR; consume(REST);",
            "let (Typed(filter), REST,) = EXPR else { return; }; consume(REST);",
            "if let (Typed(filter), REST,) = EXPR { consume(REST); } else {}",
            "match EXPR {\n    (Typed(filter), REST,) => consume(REST),\n}",
        )
        call = "parse_target_with_ctx(\n    text,\n    ctx,\n)"
        exprs = (
            call,
            "ctx.with_player_antecedents(\n    |s| parse_target_with_ctx(i, s),\n)",
            "ctx.with_player_antecedents(\n    |s| {\n"
            "        parse_target_with_ctx(\n            i,\n            s,\n        )\n    },\n)",
            "ctx.with_player_scope(\n    scope,\n"
            "    |s| parse_target_with_ctx(i, s),\n)",
            "ctx.with_player_scope(\n    scope,\n    |s| {\n"
            "        parse_target_with_ctx(\n            i,\n            s,\n        )\n    },\n)",
        )
        for template in forms:
            for expr in exprs:
                for residue, expected in (("_rest", ["binding"]), ("rest", [])):
                    source = template.replace("REST", residue).replace("EXPR", expr)
                    with self.subTest(template=template, expr=expr, residue=residue):
                        self.assertEqual([h.family for h in hits(source)], expected)
                with self.subTest(template=template, unrelated=True, expr=expr):
                    self.assertEqual(hits(template.replace("REST", "_").replace(
                        "EXPR", f"consume(\n    {expr},\n)")), [])

    def test_q_rustfmt_result_and_pattern_wrappers(self):
        for wrapper in ("Some", "Ok"):
            for trailing_pattern_comma in ("", ","):
                for residue, expected in (("_rest", ["binding"]), ("rest", [])):
                    expr = f"{wrapper}(\n    parse_target(\n        i,\n    ),\n)"
                    pattern = f"{wrapper}((filter, {residue},){trailing_pattern_comma})"
                    for template in (
                        "if let PATTERN = EXPR { consume(REST); }",
                        "match EXPR {\n    PATTERN => consume(REST),\n}",
                    ):
                        source = template.replace("PATTERN", pattern).replace(
                            "EXPR", expr).replace("REST", residue)
                        with self.subTest(wrapper=wrapper, comma=trailing_pattern_comma,
                                          residue=residue, template=template):
                            self.assertEqual([h.family for h in hits(source)], expected)

    def test_q_rustfmt_projections_and_whole_results(self):
        for helper, projection in (("parse_target", "0"),
                                   ("parse_target_with_syntax", "0"),
                                   ("parse_target_with_syntax", "2")):
            args = "i,\n    ctx," if helper == "parse_target_with_syntax" else "i,"
            call = f"{helper}(\n    {args}\n)"
            for expr in (call, f"(\n    {call}\n)"):
                with self.subTest(helper=helper, projection=projection, expr=expr):
                    self.assertEqual([h.family for h in hits(
                        f"let value = {expr}\n    .{projection};")], ["projection"])
                    self.assertEqual(hits(f"let rest = {expr}\n    .1; consume(rest);"), [])
        call = "parse_target(\n    i,\n)"
        for template in ("let _ = EXPR;", "let _: (TargetFilter, &str) = EXPR;", "EXPR;"):
            with self.subTest(template=template):
                self.assertEqual([h.family for h in hits(template.replace("EXPR", call))],
                                 ["whole-result"])
                self.assertEqual(hits(template.replace("EXPR", f"consume(\n    {call},\n)")), [])

    def test_r_known_limit_import_alias_shadowing_is_untracked(self):
        """Aliases never occur in the real parser tree; local shadowing is untracked."""
        imported = "use crate::target::parse_target as parse;\n"
        shadowed = ("let parse = |s| (s.len(), ());\n"
                    "let (n, _) = parse(i);")
        self.assertEqual([(h.family, h.symbol) for h in hits(imported + shadowed)],
                         [("binding", "parse_target")])
        self.assertEqual(hits(shadowed), [])
        self.assertEqual(hits(imported + shadowed.replace(
            "let (n, _) = parse(i);", "let (n, rest) = parse(i); consume(rest);")), [])

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
            multiline = f"{helper}(\n    input,\n)"
            for expr in (call, f"ctx.with_player_antecedents(|s| {call})",
                         f"ctx.with_player_scope(scope, |s| {{ {call} }})",
                         multiline,
                         f"ctx.with_player_antecedents(\n    |s| {multiline},\n)",
                         f"ctx.with_player_scope(\n    scope,\n    |s| {{ {multiline} }},\n)"):
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
                          "while flag { break; }", "for x in xs { observe(); }",
                          "if let Foo { a } = prior { observe(a); }",
                          "if let Foo { a } = prior { observe(a); } else { observe(); }",
                          "if let Foo { a } = prior && let Bar { b } = a { observe(b); }",
                          "while let Foo { a } = it.next() { observe(a); }",
                          "for Foo { a } in xs { observe(a); }",
                          "for (Foo { a }, 0..=3) in xs { observe(a); }",
                          "match prior { Foo { a } => observe(a), _ => {} }"):
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
                     "for x in xs { consume(rest); }",
                     "if let Foo { a } = prior { consume(rest); }",
                     "for Foo { a } in xs { consume(rest); }",
                     "while let Foo { a } = it.next() { consume(rest); break; }"):
            with self.subTest(body=body):
                prefix = "match parse_target(input) { (TargetFilter::Any, rest) => " + body
                self.assertEqual([h.family for h in hits(prefix + " (v, _) => use_it(v), }")],
                                 ["binding"])
                self.assertEqual(hits(prefix + " (v, rest) => consume(rest), }"), [])

    def test_l_turbofish_commas_do_not_end_a_match_arm_expression(self):
        # Generic-argument commas are not arm separators: the discard arm that follows
        # a turbofish-bearing arm expression must still be counted, and a consumed
        # remainder must still count nothing.
        for call in ("tag::<_, _, E>(\"tail\")",
                     "tag::<_, _, OracleError<'_>>(\"tail\")",
                     "collect::<Vec<(A, B)>>()",
                     "nom::combinator::opt::<_, _, (E, F)>(inner)",
                     "tag::<_, _, E>(\"a\").or(tag::<_, _, E>(\"b\"))"):
            with self.subTest(call=call):
                prefix = ("match parse_target(input) { (TargetFilter::Any, rest) => "
                          f"{call}.parse(rest).is_ok(), ")
                self.assertEqual([h.family for h in hits(prefix + "(v, _) => use_it(v), }")],
                                 ["binding"])
                self.assertEqual(hits(prefix + "(v, rest) => consume(rest), }"), [])
        # A comparison is not a turbofish: its `<` and `>` must not swallow the arm.
        self.assertEqual([h.family for h in hits(
            "match parse_target(input) { (TargetFilter::Any, rest) => a < b, "
            "(v, _) => use_it(v), }")], ["binding"])

    def test_block_initializer_postfix_is_not_a_bare_helper_result(self):
        # Only registered result-preserving wrappers are followed. Projections
        # and arbitrary methods (even clone) must agree with the non-block form.
        for postfix in (".1.is_empty()", ".1.clone()", ".clone()",
                        ".map(|x| { x })"):
            for pattern in ("_rest_is_empty", "(filter, _)"):
                for expr in ("parse_target(input)",
                             "{ let input = input.trim(); parse_target(input) }"):
                    with self.subTest(postfix=postfix, pattern=pattern, expr=expr):
                        self.assertEqual(hits(f"let {pattern} = {expr}{postfix};"), [])

    def test_method_projections_do_not_resolve_helper_aliases_or_names(self):
        for receiver in ("reader", "(reader)", "factory()", "readers[0]",
                         "a.b().c", "self.x"):
            for callee, imports in (
                ("read", "use crate::target::parse_target as read; "),
                ("parse_target", ""),
            ):
                for projection in ("0", "2", "1"):
                    with self.subTest(receiver=receiver, callee=callee,
                                      projection=projection):
                        self.assertEqual(hits(
                            imports + f"let rest = {receiver}.{callee}(i).{projection}; "
                            "consume(rest);"), [])

    def test_range_endpoint_projections_keep_discard_obligations(self):
        for helper, projection in (("parse_target", "0"),
                                   ("parse_target_with_syntax", "0"),
                                   ("parse_target_with_syntax", "2")):
            args = "i, ctx" if helper == "parse_target_with_syntax" else "i"
            call = f"{helper}({args})"
            for receiver in (call, f"crate::target::{call}", f"({call})",
                             f"{{ {call} }}"):
                for prefix in ("..", "a..", "..="):
                    with self.subTest(helper=helper, projection=projection,
                                      receiver=receiver, prefix=prefix):
                        self.assertEqual([(h.family, h.symbol) for h in hits(
                            f"let value = {prefix}{receiver}.{projection};")],
                            [("projection", helper)])
                        self.assertEqual(hits(
                            f"let rest = {prefix}{receiver}.1; consume(rest);"), [])

    def test_free_helper_projections_keep_discard_obligations(self):
        for call, helper, projection, imports in (
            ("read(i)", "parse_target", "0",
             "use crate::target::parse_target as read; "),
            ("parse_target(i)", "parse_target", "0", ""),
            ("crate::target::parse_target(i)", "parse_target", "0", ""),
            ("parse_target_with_syntax(i, c)", "parse_target_with_syntax", "2", ""),
        ):
            block = "{ let i = i.trim(); " + call + " }"
            for expr in (call, f"({call})", block, f"({block})"):
                for template in ("let value = EXPR; consume(value);",
                                 "fn f() -> T { return EXPR; }",
                                 "fn f() -> T { loop { break EXPR; } }"):
                    with self.subTest(call=call, expr=expr, template=template):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(
                                imports + template.replace("EXPR", f"{expr}.{projection}"))],
                            [("projection", helper)])
                        self.assertEqual(hits(
                            imports + template.replace("EXPR", f"{expr}.1")), [])

    def test_registered_wrapper_projections_keep_discard_obligations(self):
        for wrapper in ("ctx.with_player_antecedents(|s| CALL)",
                        "ctx.with_player_scope(scope, |s| { CALL })"):
            for call, helper, projection in (
                ("parse_target(i)", "parse_target", "0"),
                ("parse_target_with_syntax(i, c)", "parse_target_with_syntax", "2"),
            ):
                wrapped = wrapper.replace("CALL", call)
                for expr in (wrapped, f"({wrapped})"):
                    with self.subTest(wrapper=wrapper, call=call, expr=expr):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(
                                f"let value = {expr}.{projection}; consume(value);")],
                            [("projection", helper)])
                        self.assertEqual(hits(
                            f"let rest = {expr}.1; consume(rest);"), [])

    def test_bare_block_projections_match_parenthesized_and_call_forms(self):
        for helper, projection in (("parse_target", "0"),
                                   ("parse_target_with_syntax", "0"),
                                   ("parse_target_with_syntax", "2")):
            args = "i, ctx" if helper == "parse_target_with_syntax" else "i"
            call = f"{helper}({args})"
            block = "{ let i = input.trim(); " + call + " }"
            for expr in (block, f"({block})", call):
                for name in ("_filter", "filter"):
                    with self.subTest(helper=helper, projection=projection,
                                      expr=expr, name=name):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(
                                f"let {name} = {expr}.{projection};")],
                            [("projection", helper)])

    def test_block_projections_in_return_and_break_operands(self):
        contexts = (
            "fn f(i: &str, e: bool) -> T { if e { return EXPR; } T::Any }",
            "fn f(i: &str) -> T { loop { break EXPR; } }",
            "fn f(i: &str) -> T { 'a: loop { break 'a EXPR; } }",
        )
        for helper, projection in (("parse_target", "0"),
                                   ("parse_target_with_syntax", "2")):
            args = "i, ctx" if helper == "parse_target_with_syntax" else "i"
            call = f"{helper}({args})"
            block = "{ let i = i.trim(); " + call + " }"
            for context in contexts:
                for expr in (block, f"({block})", call):
                    with self.subTest(helper=helper, context=context, expr=expr):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(
                                context.replace("EXPR", f"{expr}.{projection}"))],
                            [("projection", helper)])

    def test_return_and_break_operands_preserve_remainder_controls(self):
        for prefix in ("return", "break", "break 'a"):
            for helper in ("parse_target", "parse_target_with_syntax"):
                args = "i, ctx" if helper == "parse_target_with_syntax" else "i"
                call = f"{helper}({args})"
                block = "{ let i = i.trim(); " + call + " }"
                for expr in (block, f"({block})", call):
                    for postfix in (".1", ".1.len()"):
                        with self.subTest(prefix=prefix, helper=helper,
                                          expr=expr, postfix=postfix):
                            self.assertEqual(hits(
                                "fn f(i: &str) { 'a: loop { "
                                f"{prefix} {expr}{postfix};"
                                " } }"), [])
                with self.subTest(prefix=prefix, helper=helper):
                    self.assertEqual(hits(
                        "fn f(i: &str) { 'a: loop { "
                        f"let (filter, rest) = {block}; consume(rest); "
                        f"{prefix} filter;"
                        " } }"), [])

    def test_bare_block_non_discard_postfixes_and_consumed_remainder(self):
        for helper in ("parse_target", "parse_target_with_syntax"):
            args = "i, ctx" if helper == "parse_target_with_syntax" else "i"
            block = "{ let i = input.trim(); " + f"{helper}({args})" + " }"
            for postfix in (".1", ".1.is_empty()", ".clone()"):
                for name in ("_rest", "rest"):
                    with self.subTest(helper=helper, postfix=postfix, name=name):
                        self.assertEqual(hits(f"let {name} = {block}{postfix};"), [])
        block = "{ let i = input.trim(); parse_target(i) }"
        self.assertEqual(hits(f"let _syntax = {block}.2;"), [])
        self.assertEqual(hits(f"let (f, rest) = {block}; consume(rest);"), [])

    def test_projection_does_not_treat_keyword_bodies_as_standalone_blocks(self):
        # These expression bodies were not helper-result receivers before bare
        # block projections were supported; the closing brace alone is not enough.
        for call, projection in (("parse_target(i)", "0"),
                                 ("parse_target_with_syntax(i, ctx)", "2")):
            for expr in ("if c { CALL } else { CALL }",
                         "match x { _ => CALL }",
                         "unsafe { CALL }",
                         "loop { CALL }",
                         "|i| -> (TargetFilter, &str) { CALL }",
                         "Foo { value: CALL }"):
                for name in ("_filter", "filter"):
                    with self.subTest(call=call, expr=expr, name=name):
                        self.assertEqual(hits(
                            f"let {name} = {expr.replace('CALL', call)}.{projection};"), [])

    def test_block_initializer_bare_and_question_mark_keep_discard_obligations(self):
        for expr in ("{ let input = input.trim(); parse_target(input) }",
                     "{ let input = input.trim(); parse_target(input) }?"):
            for pattern, expected in (("(filter, _)", ["binding"]),
                                      ("_result", ["whole-result"]),
                                      ("(filter, rest)", [])):
                with self.subTest(expr=expr, pattern=pattern):
                    self.assertEqual(
                        [h.family for h in hits(f"let {pattern} = {expr};")], expected)

    def test_block_initializer_postfix_stops_before_if_let_body_and_let_else(self):
        for template in (
            "if let PATTERN = EXPR { observe(); } else { observe(); }",
            "let PATTERN = EXPR else { return; }; observe();",
        ):
            for expr, expected in (
                ("{ let input = input.trim(); parse_target(input) }", ["binding"]),
                ("{ let input = input.trim(); parse_target(input) }?", ["binding"]),
                ("{ let input = input.trim(); parse_target(input) }.1.clone()", []),
                ("{ let input = input.trim(); parse_target(input) }.map(|x| { x })", []),
            ):
                source = template.replace("PATTERN", "(Typed(filter), _)").replace("EXPR", expr)
                with self.subTest(template=template, expr=expr):
                    self.assertEqual([h.family for h in hits(source)], expected)

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

    def test_j_aliases_only_resolve_unqualified_calls(self):
        cases = (
            ("use crate::target::parse_target as read; "
             "fn f(input: &str) { let (rest, _) = other::read(input); consume(rest); }",
             []),
            ("use other::read as parse_target; "
             "fn f(input: &str) { let (filter, _) = crate::target::parse_target(input); }",
             [("binding", "parse_target")]),
            ("use crate::target::parse_target as read; "
             "fn f(input: &str) { let (filter, _) = read(input); }",
             [("binding", "parse_target")]),
        )
        for source, expected in cases:
            with self.subTest(source=source):
                self.assertEqual([(h.family, h.symbol) for h in hits(source)], expected)
                consumed = source.replace("(filter, _) =", "(filter, rest) =").replace(
                    "input); }", "input); consume(rest); }")
                self.assertEqual(hits(consumed), [])

    def test_j_qualified_reexport_does_not_use_callers_alias(self):
        src = ("mod a { pub use crate::target::parse_target as read; }\n"
               "mod b { use crate::target::parse_target_with_syntax as read; "
               "fn f(input: &str) { let (filter, _) = crate::a::read(input); } }")
        self.assertEqual(hits(src), [])
        self.assertEqual(hits(src.replace(
            "(filter, _) = crate::a::read(input);",
            "(filter, rest) = crate::a::read(input); consume(rest);")), [])

    def test_j_qualified_helpers_ignore_alias_shadowing(self):
        for helper in sorted(rd.HELPERS):
            syntax = ", syntax" if helper == "parse_target_with_syntax" else ""
            for qualifier in ("self::", "crate::", "super::", "a::b::", "::"):
                with self.subTest(helper=helper, qualifier=qualifier):
                    prefix = f"use other::read as {helper}; fn f(input: &str) {{ "
                    call = f"{qualifier}{helper}(input, ctx);"
                    self.assertEqual(
                        [(h.family, h.symbol) for h in hits(
                            prefix + f"let (filter, _{syntax}) = " + call + " }")],
                        [("binding", helper)])
                    self.assertEqual(hits(
                        prefix + f"let (filter, rest{syntax}) = " + call
                        + " consume(rest); }"), [])

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
                    self.assertIn("detector internal error at crates/phase-parser/src/parser/probe.rs",
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

    def test_scope_wrapper_closure_return_annotations(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for annotation in ("", " -> (TargetFilter, &'a str)",
                               " -> (TargetFilter, &str)",
                               " -> Result<(TargetFilter, &'a str), E>",
                               " -> Option<Vec<(TargetFilter, &'a str)>>",
                               " -> Result<fn() -> (TargetFilter, &'a str), E>"):
                for capture in ("", "move "):
                    for statement in ("", "let input = prepare(input); observe(); "):
                        expr = (f"ctx.{wrapper}({prefix}{capture}|s|{annotation} {{ "
                                f"{statement}parse_target_with_ctx(input, s) }},)")
                        with self.subTest(wrapper=wrapper, annotation=annotation,
                                          capture=capture, statement=statement):
                            self.assertEqual(
                                [(h.family, h.symbol) for h in hits(f"let (filter, _) = {expr};")],
                                [("binding", "parse_target_with_ctx")])
                            self.assertEqual(
                                hits(f"let (filter, rest) = {expr}; consume(rest);"), [])
            expr = f"ctx.{wrapper}({prefix}|s| -> u8 {{ 0 }})"
            with self.subTest(wrapper=wrapper, result="not a helper"):
                self.assertEqual(hits(f"let (filter, _) = {expr};"), [])

    def test_scope_wrapper_compound_receivers(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for receiver in ("contexts[0]", "get_context()", "self.ctx().inner[0]",
                             "get_context()?"):
                for closure in ("|s| parse_target_with_ctx(input, s)",
                                "move |s| -> Result<(TargetFilter, &'a str), E> "
                                "{ parse_target_with_ctx(input, s) },"):
                    expr = f"{receiver}.{wrapper}({prefix}{closure})"
                    with self.subTest(wrapper=wrapper, receiver=receiver, closure=closure,
                                      result="discarded remainder"):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(f"let (filter, _) = {expr};")],
                            [("binding", "parse_target_with_ctx")])
                    with self.subTest(wrapper=wrapper, receiver=receiver, closure=closure,
                                      result="consumed remainder"):
                        self.assertEqual(
                            hits(f"let (filter, rest) = {expr}; consume(rest);"), [])

    def test_scope_wrapper_compound_receiver_projections(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for receiver in ("contexts[0]", "get_context()", "get_context()?",
                             "self.ctx().inner[0]"):
                for helper in ("parse_target_with_ctx", "parse_target_with_syntax"):
                    wrapped = f"{receiver}.{wrapper}({prefix}|s| {helper}(input, s))"
                    for expr in (wrapped, f"({wrapped})"):
                        for projection in (("0", "2") if helper == "parse_target_with_syntax"
                                           else ("0",)):
                            with self.subTest(wrapper=wrapper, receiver=receiver,
                                              helper=helper, expr=expr, projection=projection):
                                self.assertEqual(
                                    [(h.family, h.symbol) for h in hits(
                                        f"let f = {expr}.{projection};")],
                                    [("projection", helper)])
                        with self.subTest(wrapper=wrapper, receiver=receiver,
                                          helper=helper, expr=expr, projection="1"):
                            self.assertEqual(hits(f"let rest = {expr}.1; consume(rest);"), [])
                expr = f"{receiver}.with_other(|s| parse_target_with_ctx(input, s)).0"
                with self.subTest(control=expr):
                    self.assertEqual(hits(f"let f = {expr};"), [])
                # Tuple projection is postfix: it binds before the binary operator.
                for operator in ("+", "-", "*", "/", "%", "&", "|", "^",
                                 "<", ">", "==", "!=", "&&", "||"):
                    expr = (f"a {operator} {receiver}.{wrapper}({prefix}|s| "
                            "parse_target_with_ctx(input, s)).0")
                    with self.subTest(operator=operator, expr=expr):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(f"let f = {expr};")],
                            [("projection", "parse_target_with_ctx")])

    def test_scope_wrapper_projection_keyword_boundaries(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for receiver in ("contexts[0]", "get_context()", "get_context()?",
                             "self.ctx().inner[0]", "(ctx)"):
                expr = (f"{receiver}.{wrapper}({prefix}|s| "
                        "parse_target_with_ctx(input, s)).0")
                for template in ("if EXPR {}", "match EXPR { _ => () }",
                                 "return EXPR;", "for f in EXPR {}"):
                    with self.subTest(wrapper=wrapper, receiver=receiver, template=template):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(template.replace("EXPR", expr))],
                            [("projection", "parse_target_with_ctx")])

    def test_scope_wrapper_compound_receiver_guards(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for receiver in ("a + ctx", "x == ctx"):
                expr = f"{receiver}.{wrapper}({prefix}|s| parse_target_with_ctx(input, s))"
                with self.subTest(wrapper=wrapper, receiver=receiver):
                    self.assertEqual(hits(f"let (filter, _) = {expr};"), [])
            for receiver in ("contexts[0]", "get_context()", "self.ctx().inner[0]",
                             "get_context()?"):
                expr = f"{receiver}.{wrapper}({prefix}|s| -> u8 {{ 0 }})"
                with self.subTest(wrapper=wrapper, receiver=receiver, result="not a helper"):
                    self.assertEqual(hits(f"let (filter, _) = {expr};"), [])

    def test_scope_wrapper_turbofish_results(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for receiver in ("ctx", "contexts[0]", "get_context::<T>()",
                             "Self::get_context::<Vec<(A, B)>>()",
                             "get_context::<fn() -> T>()"):
                for generic in ("", "::<_>", "::<Vec<(A, B)>>", "::<fn() -> T>"):
                    expr = (f"{receiver}.{wrapper}{generic}({prefix}|s| "
                            "parse_target_with_ctx(input, s))")
                    for template, expected in (
                        ("let (f, _) = EXPR;", [("binding", "parse_target_with_ctx")]),
                        ("let (f, rest) = EXPR; consume(rest);", []),
                        ("let f = EXPR.0;", [("projection", "parse_target_with_ctx")]),
                        ("let rest = EXPR.1; consume(rest);", []),
                    ):
                        with self.subTest(wrapper=wrapper, receiver=receiver,
                                          generic=generic, template=template):
                            self.assertEqual(
                                [(h.family, h.symbol) for h in hits(
                                    template.replace("EXPR", expr))], expected)
                    unregistered = expr.replace(wrapper, "with_other")
                    for template in ("let (f, _) = EXPR;", "let f = EXPR.0;"):
                        with self.subTest(unregistered=unregistered, template=template):
                            self.assertEqual(hits(template.replace("EXPR", unregistered)), [])

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

    def test_block_arm_followed_by_or_pattern_starting_with_a_tuple(self):
        for src, expected in (
            ("match parse_target(i) { (Typed(_), rest) => { consume(rest); } "
             "(Any, _) | (SelfRef, _) => {} (_, rest) => { consume(rest); } }", 2),
            ("match parse_target(i) { (Typed(_), rest) => { consume(rest); } "
             "(Any, _) | (SelfRef, _) if keep() => {} (_, rest) => { consume(rest); } }", 2),
            ("match parse_target(i) { (Typed(_), rest) => { consume(rest); } "
             "[Any, _] | [SelfRef, _] => {} }", 0),
            ("match parse_target(i) { (Typed(_), rest) => { consume(rest); } "
             "(Any, r) | (SelfRef, r) => { consume(r); } }", 0),
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

    def test_t_exact_test_filenames_preserve_production_sources(self):
        discard = "fn f() { let (v, _) = parse_target(i); }"
        found = self.discovery_hits({
            "mod.rs": "mod contests;\nmod latest_tests_helper;\n"
                      "#[cfg(test)]\nmod support;",
            "contests.rs": discard,
            "latest_tests_helper.rs": discard,
            "tests.rs": discard,
            "foo_tests.rs": discard,
            "support.rs": discard,
            "inner.rs": "#![cfg(test)]\n" + discard,
            "snapshots/fake.rs": discard,
            "production.rs": discard,
        })
        self.assertEqual(sorted(found), [
            ("contests.rs", "binding"),
            ("latest_tests_helper.rs", "binding"),
            ("production.rs", "binding"),
        ])

    def test_children_outlined_under_exact_test_filenames_stay_test_only(self):
        discard = "fn f() { let _parsed = parse_target(i); }"
        for owner, directory in (("tests.rs", "tests"), ("unit_tests.rs", "unit_tests")):
            with self.subTest(owner=owner):
                found = self.discovery_hits({
                    "mod.rs": "#[cfg(test)]\nmod " + owner[:-3] + ";\nmod witness;",
                    owner: "mod support;",
                    directory + "/support.rs": "mod deeper;\n" + discard,
                    directory + "/support/deeper.rs": discard,
                    "witness.rs": discard,
                })
                self.assertEqual(sorted(found), [("witness.rs", "whole-result")])

    def test_production_reference_wins_over_children_of_exact_test_filenames(self):
        discard = "fn f() { let _parsed = parse_target(i); }"
        found = self.discovery_hits({
            "mod.rs": '#[cfg(test)]\nmod tests;\n#[path = "tests/support.rs"]\nmod shared;',
            "tests.rs": "mod support;",
            "tests/support.rs": discard,
        })
        self.assertEqual(found, [("tests/support.rs", "whole-result")])

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
