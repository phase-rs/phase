#!/usr/bin/env python3
"""Tests for the CR 733 P0 mutation/reachability census generator.

The generator (`cr733_mutation_census.py`) is an over-approximate inventory, so
the risk is not that it misses a subtle case but that a detector fires on the
wrong text (a false site) or that a structural parser (the GameState field span,
the call graph) silently mis-scopes. These tests pin every detector and the two
structural parsers against synthetic inline Rust, and drive the shared scanner so
that comments / strings / `#[cfg(test)]` bodies are proven out of scope.

Run:  python3 scripts/cr733_mutation_census_tests.py
"""

from __future__ import annotations

import unittest

import cr733_mutation_census as census
from zone_authority_census import CensusError, iter_production_lines


def build_fields(*names: str) -> tuple[frozenset[str], dict]:
    """A field set plus its compiled write-regex bundle, for site detection."""
    fields = list(names)
    return frozenset(fields), census.build_write_regexes(fields)


def scan(src: str, fn_names=frozenset(), fields=("a",), reveal=frozenset()):
    """Run the generator's per-line detection over synthetic source in `t.rs`.

    Returns the list of `Site` objects (reachability NOT yet annotated).
    """
    fields_set, write_res = build_fields(*fields)
    lines = src.splitlines()
    sites: list[census.Site] = []
    edges: list[tuple[str, str]] = []
    unresolved = 0
    in_destructure = False
    for i, _raw, code, fn in iter_production_lines("t.rs", lines):
        decl_m = census.FN_DECL.match(code)
        decl_name = decl_m.group(1) if decl_m else None
        line_edges, line_unresolved = census.extract_call_edges(fn_names, code, fn, decl_name)
        edges.extend(line_edges)
        unresolved += line_unresolved
        # Mirror scan_file's destructure carrier so multi-line binding lists work.
        if in_destructure or census.DESTRUCTURE_OPEN.search(code):
            fragment = code
            if not in_destructure:
                m = census.DESTRUCTURE_OPEN.search(code)
                fragment = code[m.end():]
                in_destructure = True
            close = fragment.find("}")
            binding = fragment if close == -1 else fragment[:close]
            for name in census.IDENT.findall(binding):
                if name in fields_set:
                    sites.append(census.Site("write", "t.rs", i + 1, fn, "destructure", field=name, receiver="GameState"))
                if name == "rng":
                    sites.append(census.Site("rng", "t.rs", i + 1, fn, "destructured-rng"))
            if close != -1:
                in_destructure = False
        for label, field, recv in census.detect_write_sites(code, write_res):
            sites.append(census.Site("write", "t.rs", i + 1, fn, label, field=field, receiver=recv))
        for label, regex in census.RNG_PATTERNS:
            if regex.search(code):
                sites.append(census.Site("rng", "t.rs", i + 1, fn, label))
        for alloc in census.detect_allocators(code):
            sites.append(census.Site("allocator", "t.rs", i + 1, fn, "next-id-alloc", variant=alloc))
        if census.EVENT_EMIT.search(code):
            sites.append(census.Site("event_emission", "t.rs", i + 1, fn, "events-push", variant=census.extract_event_variant(code)))
        for name in census.iter_call_tokens(code, decl_name):
            if name in reveal:
                sites.append(census.Site("information", "t.rs", i + 1, fn, "reveal-call", variant=name))
    return sites, edges, unresolved


def fields_of(sites, family):
    return sorted(s.field for s in sites if s.family == family and s.field)


def patterns_of(sites, family):
    return sorted(s.pattern for s in sites if s.family == family)


class GameStateFieldParsingTests(unittest.TestCase):
    """The struct field parser must key on `pub NAME:` at brace depth 1 only, and
    survive attrs, doc comments, cfg gates, and nested generics."""

    def test_basic_fields(self) -> None:
        src = (
            "pub struct GameState {\n"
            "    pub turn_number: u32,\n"
            "    pub players: Vec<Player>,\n"
            "}\n"
        )
        self.assertEqual(census.parse_gamestate_fields(src.splitlines()), ["turn_number", "players"])

    def test_attrs_and_doc_comments_are_not_fields(self) -> None:
        src = (
            "pub struct GameState {\n"
            "    /// The active player's id.\n"
            "    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n"
            "    pub active_player: PlayerId,\n"
            "    #[cfg(feature = \"x\")]\n"
            "    pub gated: bool,\n"
            "}\n"
        )
        self.assertEqual(census.parse_gamestate_fields(src.splitlines()), ["active_player", "gated"])

    def test_nested_generics_do_not_leak_inner_types(self) -> None:
        # A field whose type carries its own braces/angles must yield exactly one
        # field name, not the inner type tokens.
        src = (
            "pub struct GameState {\n"
            "    pub map: HashMap<PlayerId, Vec<Foo<Bar>>>,\n"
            "    pub tally: BTreeMap<Zone, u32>,\n"
            "}\n"
        )
        self.assertEqual(census.parse_gamestate_fields(src.splitlines()), ["map", "tally"])

    def test_brace_span_stops_at_struct_close(self) -> None:
        # A `pub NAME:` in a following impl/struct must not be counted: the span
        # ends when GameState's own brace closes.
        src = (
            "pub struct GameState {\n"
            "    pub kept: u32,\n"
            "}\n"
            "pub struct Other {\n"
            "    pub ignored: u32,\n"
            "}\n"
        )
        self.assertEqual(census.parse_gamestate_fields(src.splitlines()), ["kept"])

    def test_private_and_nested_struct_fields_excluded(self) -> None:
        # Only `pub` fields at depth 1; a private field or a field of an inline
        # nested type literal must not appear.
        src = (
            "pub struct GameState {\n"
            "    pub visible: u32,\n"
            "    private: u32,\n"
            "}\n"
        )
        self.assertEqual(census.parse_gamestate_fields(src.splitlines()), ["visible"])

    def test_missing_struct_returns_empty(self) -> None:
        self.assertEqual(census.parse_gamestate_fields(["fn f() {}"]), [])


class WriteSiteTests(unittest.TestCase):
    """Assignment, compound-assignment, mutating methods, `&mut` borrows, and
    non-`state` receivers -- with comparisons rejected."""

    def test_plain_assignment(self) -> None:
        sites, _e, _u = scan("fn f() { state.phase = next; }", fields=("phase",))
        self.assertIn("assign", patterns_of(sites, "write"))
        self.assertEqual(fields_of(sites, "write"), ["phase"])

    def test_compound_assignment(self) -> None:
        sites, _e, _u = scan("fn f() { state.turn_number += 1; state.life -= 2; }", fields=("turn_number", "life"))
        self.assertEqual(patterns_of(sites, "write"), ["assign", "assign"])
        self.assertEqual(fields_of(sites, "write"), ["life", "turn_number"])

    def test_comparisons_are_not_writes(self) -> None:
        # `==`, `!=`, `<=`, `>=` must NOT register as assignments.
        for op in ("==", "!=", "<=", ">="):
            with self.subTest(op=op):
                sites, _e, _u = scan(f"fn f() {{ if state.turn_number {op} 3 {{}} }}", fields=("turn_number",))
                self.assertEqual([s for s in sites if s.family == "write"], [])

    def test_fat_arrow_is_not_a_write(self) -> None:
        # `field =>` (a match-arm fat arrow) must not read as `field =` -- the real
        # miss the `(?![=>])` guard closes (engine.rs:1225-style scrutinee arm).
        sites, _e, _u = scan("fn f() { match k { x.phase => go(), } }", fields=("phase",))
        self.assertEqual([s for s in sites if s.family == "write"], [])

    def test_index_element_assignment(self) -> None:
        # `state.field[i] = / += / -=` -- the real misses at skip_next_turn.rs:54
        # and turns.rs:666/761 (`turns_to_skip`). The index must not blind the
        # assign detector.
        src = (
            "fn f() {\n"
            "    state.turns_to_skip[i] = 1;\n"
            "    state.turns_to_skip[p] += 1;\n"
            "    state.tally[k] -= 2;\n"
            "}\n"
        )
        sites, _e, _u = scan(src, fields=("turns_to_skip", "tally"))
        self.assertEqual(patterns_of(sites, "write"), ["assign", "assign", "assign"])
        self.assertEqual(fields_of(sites, "write"), ["tally", "turns_to_skip", "turns_to_skip"])

    def test_multi_index_assignment(self) -> None:
        sites, _e, _u = scan("fn f() { state.grid[i][j] = v; }", fields=("grid",))
        self.assertEqual(patterns_of(sites, "write"), ["assign"])

    def test_mutating_methods(self) -> None:
        sites, _e, _u = scan(
            "fn f() { state.stack.push(x); state.objects.get_mut(&id); state.hand.retain(|c| keep(c)); }",
            fields=("stack", "objects", "hand"),
        )
        self.assertEqual(
            patterns_of(sites, "write"),
            ["mut_method:get_mut", "mut_method:push", "mut_method:retain"],
        )

    def test_iter_and_values_mut_methods(self) -> None:
        # The `*_mut` iterators are mutation surfaces (real misses at
        # casting.rs:14015, draw.rs:406, choose.rs:241). All four `*_mut` accessors
        # must classify.
        src = (
            "fn f() {\n"
            "    for o in state.objects.values_mut() {}\n"
            "    for c in state.hand.iter_mut() {}\n"
            "    let a = state.stack.last_mut();\n"
            "    let b = state.stack.first_mut();\n"
            "}\n"
        )
        sites, _e, _u = scan(src, fields=("objects", "hand", "stack"))
        self.assertEqual(
            patterns_of(sites, "write"),
            ["mut_method:first_mut", "mut_method:iter_mut", "mut_method:last_mut", "mut_method:values_mut"],
        )

    def test_future_proofed_mutators(self) -> None:
        # #3 additions: currently 0 hits, but a future field mutation through any
        # of these must classify rather than slip through silently.
        src = (
            "fn f() {\n"
            "    state.stack.swap_remove(0);\n"
            "    state.hand.split_off(2);\n"
            "    state.tally.extend_from_slice(&more);\n"
            "    state.buf.resize(4, 0);\n"
            "    state.buf.dedup();\n"
            "    state.flags.set(k, true);\n"
            "}\n"
        )
        sites, _e, _u = scan(src, fields=("stack", "hand", "tally", "buf", "flags"))
        self.assertEqual(
            patterns_of(sites, "write"),
            [
                "mut_method:dedup",
                "mut_method:extend_from_slice",
                "mut_method:resize",
                "mut_method:set",
                "mut_method:split_off",
                "mut_method:swap_remove",
            ],
        )

    def test_mut_borrow(self) -> None:
        sites, _e, _u = scan("fn f() { shuffle(&mut state.library); }", fields=("library",))
        self.assertEqual(patterns_of(sites, "write"), ["borrow"])
        self.assertEqual(fields_of(sites, "write"), ["library"])

    def test_non_state_receiver_recorded(self) -> None:
        # A receiver other than `state` is matched (over-report) but its name is
        # recorded so review can filter it.
        sites, _e, _u = scan("fn f() { simulated.phase = p; clone.stack.push(x); }", fields=("phase", "stack"))
        recvs = sorted((s.receiver, s.field) for s in sites if s.family == "write")
        self.assertEqual(recvs, [("clone", "stack"), ("simulated", "phase")])

    def test_single_line_destructure(self) -> None:
        sites, _e, _u = scan("fn f() { let GameState { players, rng, .. } = state; }", fields=("players", "objects"))
        self.assertIn(("write", "players"), [(s.family, s.field) for s in sites])
        # `rng` is not a declared field here but the destructure still flags the
        # RNG binding.
        self.assertIn("rng", [s.family for s in sites])

    def test_multiline_destructure(self) -> None:
        src = (
            "fn f() {\n"
            "    let GameState {\n"
            "        players,\n"
            "        objects,\n"
            "        rng,\n"
            "        ..\n"
            "    } = state;\n"
            "}\n"
        )
        sites, _e, _u = scan(src, fields=("players", "objects"))
        self.assertEqual(sorted(s.field for s in sites if s.family == "write"), ["objects", "players"])
        self.assertIn("rng", [s.family for s in sites])


class RngTests(unittest.TestCase):
    def test_rng_patterns(self) -> None:
        src = (
            "fn f() {\n"
            "    state.rng.foo();\n"
            "    deck.shuffle(&mut rng);\n"
            "    let c = cards.choose(&mut state.rng);\n"
            "    let b = rng.random_bool(0.5);\n"
            "    let n = rng.random_range(0..3);\n"
            "}\n"
        )
        sites, _e, _u = scan(src)
        got = set(patterns_of(sites, "rng"))
        # state.rng fires on two lines; the ops fire on their own lines.
        self.assertIn("state.rng", got)
        self.assertIn("shuffle", got)
        self.assertIn("choose", got)
        self.assertIn("random_bool", got)
        self.assertIn("random_range", got)


class AllocatorTests(unittest.TestCase):
    def test_generic_next_id_and_timestamp(self) -> None:
        src = (
            "fn f() {\n"
            "    let a = state.next_object_id();\n"
            "    let b = state.next_pip_id();\n"
            "    let t = state.next_timestamp();\n"
            "    let z = seq.next_frame_id();\n"
            "    let future = stack.next_resolution_command_id();\n"
            "}\n"
        )
        sites, _e, _u = scan(src)
        self.assertEqual(
            sorted(s.variant for s in sites if s.family == "allocator"),
            ["next_frame_id", "next_object_id", "next_pip_id", "next_resolution_command_id", "next_timestamp"],
        )


class EventEmissionTests(unittest.TestCase):
    def test_direct_push_records_variant(self) -> None:
        sites, _e, _u = scan("fn f() { events.push(GameEvent::PermanentTapped { id }); }")
        ev = [s for s in sites if s.family == "event_emission"]
        self.assertEqual(len(ev), 1)
        self.assertEqual(ev[0].variant, "PermanentTapped")

    def test_qualified_receiver_push(self) -> None:
        # `state.events.push(` is an emission just like a bare `events.push(`.
        sites, _e, _u = scan("fn f() { state.events.push(GameEvent::Foo); }")
        self.assertEqual([s.variant for s in sites if s.family == "event_emission"], ["Foo"])

    def test_extend_without_literal_variant(self) -> None:
        sites, _e, _u = scan("fn f() { events.extend(more_events); }")
        ev = [s for s in sites if s.family == "event_emission"]
        self.assertEqual(len(ev), 1)
        self.assertIsNone(ev[0].variant)


class InformationTests(unittest.TestCase):
    def test_reveal_call_is_information(self) -> None:
        reveal = frozenset({"reveal_top_card"})
        sites, _e, _u = scan("fn f() { reveal_top_card(state, p); }", reveal=reveal)
        info = [s for s in sites if s.family == "information"]
        self.assertEqual([s.variant for s in info], ["reveal_top_card"])

    def test_reveal_declaration_is_not_a_call(self) -> None:
        # The fn's own declaration line must not count as a self-call.
        reveal = frozenset({"reveal_top_card"})
        sites, _e, _u = scan("fn reveal_top_card(state: &mut GameState) { do_it(); }", reveal=reveal)
        self.assertEqual([s for s in sites if s.family == "information"], [])


class NonCodeExclusionTests(unittest.TestCase):
    """Every detector runs on the shared scanner's stripped `code`, so mutations
    named inside comments, strings, or `#[cfg(test)]` bodies must not register."""

    def test_no_write_inside_cfg_test_body(self) -> None:
        src = (
            "fn prod() { state.phase = p; }\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    fn t() { state.phase = q; }\n"
            "}\n"
            "fn prod2() { state.stack.push(x); }\n"
        )
        sites, _e, _u = scan(src, fields=("phase", "stack"))
        # Exactly the two production writes, never the test-body one.
        writes = [(s.fn, s.field, s.pattern) for s in sites if s.family == "write"]
        self.assertIn(("prod", "phase", "assign"), writes)
        self.assertIn(("prod2", "stack", "mut_method:push"), writes)
        self.assertEqual(len(writes), 2)

    def test_no_detection_inside_comment(self) -> None:
        sites, _e, _u = scan("fn f() { let x = 1; // state.phase = p; events.push(GameEvent::X);\n}", fields=("phase",))
        self.assertEqual(sites, [])

    def test_no_detection_inside_string(self) -> None:
        sites, _e, _u = scan('fn f() { log("state.phase = p and events.push(GameEvent::X)"); }', fields=("phase",))
        self.assertEqual([s for s in sites if s.family in ("write", "event_emission")], [])

    def test_desync_still_raises(self) -> None:
        src = "#[cfg(test)]\nmod tests {\n    fn t() { } } }\n"
        with self.assertRaises(CensusError):
            scan(src)


class CallTokenExclusionTests(unittest.TestCase):
    """`iter_call_tokens` is the sole tokenizer behind both call edges and the
    `unresolved_calls` metric. These pin the non-call shapes that used to pollute
    the metric (`let (`, `pub(crate)`, `#[serde(..)]`, `#[cfg(..)]`) while proving
    a genuine call still survives."""

    def tokens(self, code: str, decl_name=None) -> list[str]:
        return list(census.iter_call_tokens(code, decl_name))

    def test_let_tuple_binding_excluded(self) -> None:
        self.assertEqual(self.tokens("let (a, b) = compute(x);"), ["compute"])

    def test_pub_crate_visibility_excluded(self) -> None:
        toks = self.tokens("pub(crate) fn helper() {}", decl_name="helper")
        self.assertNotIn("pub", toks)
        self.assertNotIn("crate", toks)  # `crate)` is followed by `)`, not `(`

    def test_serde_attribute_excluded(self) -> None:
        self.assertEqual(self.tokens('#[serde(default, skip_serializing_if = "X::is_none")]'), [])

    def test_cfg_attribute_excluded(self) -> None:
        self.assertEqual(self.tokens("#[cfg(all(test, feature = something))]"), [])

    def test_allow_inner_attribute_excluded(self) -> None:
        self.assertEqual(self.tokens("#![allow(clippy::too_many_lines)]"), [])

    def test_genuine_call_still_yielded_next_to_attr(self) -> None:
        # An attribute on the same line as a call must not swallow the real call.
        self.assertEqual(self.tokens("#[inline] fn f() { do_it(x); }", decl_name="f"), ["do_it"])


class CallGraphTests(unittest.TestCase):
    """Edge extraction, unresolved counting, and BFS reachability over a two-file
    synthetic function set."""

    def test_edges_and_unresolved(self) -> None:
        fn_names = frozenset({"root", "callee", "leaf"})
        # `root` calls `callee` (known) and `unknown_ext` (unresolved) and a
        # method `.push` (unresolved). `for (` and the decl name are skipped.
        src = (
            "fn root() {\n"
            "    for (a, b) in xs {}\n"
            "    callee();\n"
            "    unknown_ext();\n"
            "    v.push(x);\n"
            "}\n"
        )
        _s, edges, unresolved = scan(src, fn_names=fn_names)
        self.assertIn(("root", "callee"), edges)
        self.assertNotIn(("root", "root"), edges)  # decl name not a self-edge
        self.assertEqual(unresolved, 2)  # unknown_ext + push

    def test_bfs_reachability(self) -> None:
        # root -> a -> b ; c is unreachable ; d only reached via b.
        adjacency = {"root": {"a"}, "a": {"b"}, "b": {"d"}, "c": {"d"}}
        reachable = census.bfs_reachable(adjacency, {"root"})
        self.assertEqual(reachable, {"root", "a", "b", "d"})
        self.assertNotIn("c", reachable)

    def test_bfs_handles_cycles(self) -> None:
        adjacency = {"root": {"a"}, "a": {"b"}, "b": {"a"}}  # a<->b cycle
        self.assertEqual(census.bfs_reachable(adjacency, {"root"}), {"root", "a", "b"})

    def test_two_file_name_over_approximation(self) -> None:
        # A call to `helper` reaches the node named `helper` regardless of which
        # file declares it -- the documented name-based over-approximation.
        file_a = "fn root() {\n    helper();\n}\n"
        file_b = "fn helper() {\n    sink();\n}\n"
        fn_names = frozenset({"root", "helper", "sink"})
        _sa, edges_a, _ua = scan(file_a, fn_names=fn_names)
        _sb, edges_b, _ub = scan(file_b, fn_names=fn_names)
        adjacency: dict[str, set[str]] = {}
        for s, d in edges_a + edges_b:
            adjacency.setdefault(s, set()).add(d)
        self.assertEqual(census.bfs_reachable(adjacency, {"root"}), {"root", "helper", "sink"})


class ReachabilityAnnotationTests(unittest.TestCase):
    def test_site_reachable_flag(self) -> None:
        sites = [
            census.Site("write", "t.rs", 1, "in_closure", "assign", field="a"),
            census.Site("write", "t.rs", 2, "outside", "assign", field="b"),
        ]
        annotated = census.annotate_reachability(sites, {"in_closure"})
        flags = {s.fn: s.reachable for s in annotated}
        self.assertTrue(flags["in_closure"])
        self.assertFalse(flags["outside"])


class DeterminismTests(unittest.TestCase):
    """Same census dict -> identical canonical JSON -> identical hash."""

    def _fixture(self) -> dict:
        return {
            "schema": census.SCHEMA,
            "head_commit": "deadbeef",
            "reachability_mode": census.REACHABILITY_MODE,
            "scope": census.SCOPE,
            "roots": [{"fn": "resolve_effect", "file": "x.rs", "line": 10}],
            "functions_indexed": 3,
            "reachable_functions": 2,
            "unresolved_calls": 7,
            "gamestate_fields": 273,
            "sites": [
                {"family": "write", "file": "b.rs", "line": 2, "fn": "g", "pattern": "assign", "field": "x", "reachable": True},
                {"family": "rng", "file": "a.rs", "line": 9, "fn": "h", "pattern": "shuffle", "reachable": False},
            ],
            "summary": {"per_family_counts": {"write": 1, "rng": 1}, "per_field_write_counts": {"x": 1}},
        }

    def test_identical_input_identical_hash(self) -> None:
        self.assertEqual(census.content_hash(self._fixture()), census.content_hash(self._fixture()))

    def test_key_order_does_not_change_hash(self) -> None:
        a = self._fixture()
        b = {k: a[k] for k in reversed(list(a.keys()))}  # same content, different insertion order
        self.assertEqual(census.content_hash(a), census.content_hash(b))

    def test_canonical_json_is_sorted_and_newline_terminated(self) -> None:
        out = census.canonical_json(self._fixture())
        self.assertTrue(out.endswith("\n"))
        self.assertLess(out.index('"functions_indexed"'), out.index('"schema"'))  # sorted keys


class RootDriftGuardTests(unittest.TestCase):
    """A vanished root must raise CensusError rather than silently shrink the
    reachable closure."""

    def test_missing_root_raises(self) -> None:
        original = census.ROOTS
        census.ROOTS = original + (("crates/engine/src/game/costs.rs", "fn_that_does_not_exist_xyz"),)
        try:
            with self.assertRaises(CensusError):
                census.find_root_locations()
        finally:
            census.ROOTS = original

    def test_all_declared_roots_exist(self) -> None:
        # The real roots must resolve at HEAD (this doubles as a live drift check).
        roots = census.find_root_locations()
        self.assertEqual(len(roots), len(census.ROOTS))
        for root in roots:
            self.assertGreater(root["line"], 0)

    def test_min_gamestate_fields_guard(self) -> None:
        # The real struct must parse well above the drift floor.
        self.assertGreaterEqual(len(census.collect_gamestate_fields()), census.MIN_GAMESTATE_FIELDS)


class FluentChainJoinTests(unittest.TestCase):
    """rustfmt-split method chains must be matched as one logical statement."""

    def _rows(self, code_lines: "list[str]", fn: str = "record_spell_cast"):
        return [(i, line, line, fn) for i, line in enumerate(code_lines)]

    def test_joins_continuation_lines_into_first_line(self) -> None:
        rows = self._rows(
            [
                "    state",
                "        .spells_cast_this_game_by_player",
                "        .entry(player)",
                "        .or_default()",
                "        .push_back(record);",
            ]
        )
        joined = census.join_fluent_chains(rows)
        self.assertEqual(len(joined), 1)
        self.assertEqual(joined[0][0], 0)  # keeps the first line's number
        wr = census.build_write_regexes(["spells_cast_this_game_by_player"])
        hits = census.detect_write_sites(joined[0][2], wr)
        self.assertEqual(
            hits, [("mut_method:entry", "spells_cast_this_game_by_player", "state")]
        )

    def test_does_not_join_across_functions(self) -> None:
        rows = [
            (0, "state", "state", "fn_a"),
            (1, "    .field(x);", "    .field(x);", "fn_b"),
        ]
        self.assertEqual(len(census.join_fluent_chains(rows)), 2)

    def test_plain_statements_pass_through_unchanged(self) -> None:
        rows = self._rows(["let a = 1;", "state.life = 3;"])
        self.assertEqual(census.join_fluent_chains(rows), rows)


class ScanPopulationTests(unittest.TestCase):
    """Population regressions: production authorities must be in the scan set."""

    def test_dual_declared_zone_authority_is_scanned(self) -> None:
        # game/zones.rs is visibility-gated (pub under test-support, pub(crate)
        # in production) but always compiled — excluding it silently deletes
        # the core zone-mutation authority from the census.
        names = {path.name for path in census.production_rs_files()}
        self.assertIn("zones.rs", names)
        self.assertIn("zone_pipeline.rs", names)


if __name__ == "__main__":
    unittest.main(verbosity=2)
