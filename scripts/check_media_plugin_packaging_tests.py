#!/usr/bin/env python3
"""Tests for check_media_plugin_packaging.py.

The script is a packaging gate, so what matters is not that it runs but that it
refuses what it should. Each case builds a throwaway tree holding all three
files the checker reads, and points the real script at it through
MEDIA_PACKAGING_ROOT.

Every fixture materialises all three targets even when the case under test
concerns only one of them. A partial tree would make the checker refuse for a
reason the test did not intend, and a refusal that arrives for the wrong reason
proves nothing about the property being tested. `tauri.conf.json` carries both
of its consumers' data for the same reason: the `.deb` depends list and the
AppImage's `bundleMediaFramework` premise, either of which can refuse.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check_media_plugin_packaging.py"
REAL_MEDIA_STACK = (Path(__file__).resolve().parent.parent
                    / "client/src-tauri/src/media_stack.rs")

DEFAULT_PACKAGES = (
    "gstreamer1.0-plugins-base",
    "gstreamer1.0-plugins-good",
    "gstreamer1.0-libav",
    "libgstreamer1.0-0",
)
DEFAULT_DECL = "pub const REQUIRED_PLUGINS: &[RequiredPlugin] = &["
# The real job and step names the checker is coupled to, so a case can hand the
# *same* name to the wrong job and prove the coupling is to both, not to one.
APT_STEP = "Install system dependencies (Linux)"
PREFLIGHT_STEP = "Install Linux Tauri build deps"
# Named in a fixture's authority and nowhere in the real tree, so a checker that
# read this checkout instead of the fixture cannot produce it.
SENTINEL = "gstreamer1.0-fixture-sentinel"


def indent(body: str, width: int) -> str:
    """A run body as a YAML block scalar's lines. Continuations keep their own
    relative indent, so a wrapped package list still reads as one command."""
    pad = " " * width
    return "\n".join(f"{pad}{line}" for line in body.splitlines())


def plugin_entry(package: str, index: int) -> str:
    return (f"    RequiredPlugin {{\n"
            f'        library: "libgstfixture{index}.so",\n'
            f'        provides: "whatever element {index} registers",\n'
            f'        debian_package: "{package}",\n'
            f"    }},")


def media_stack_source(packages: tuple[str, ...] = DEFAULT_PACKAGES,
                       *, decl: str = DEFAULT_DECL,
                       trailing: str = "") -> str:
    entries = "\n".join(plugin_entry(p, i) for i, p in enumerate(packages))
    if trailing:
        entries = f"{entries}\n{trailing}"
    return ("//! Fixture stand-in for the real media stack module.\n\n"
            "pub struct RequiredPlugin {\n"
            "    pub library: &'static str,\n"
            "    pub provides: &'static str,\n"
            "    pub debian_package: &'static str,\n"
            "}\n\n"
            f"{decl}\n{entries}\n];\n")


class PackagingTree:
    """A throwaway tree holding all three files the checker reads."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.write_media_stack()
        self.write_tauri_conf()
        self.write_workflow()

    def _write(self, rel: str, body: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")

    def write_media_stack(self, packages: tuple[str, ...] = DEFAULT_PACKAGES,
                          **kwargs: str) -> None:
        self._write("client/src-tauri/src/media_stack.rs",
                    media_stack_source(packages, **kwargs))

    def write_tauri_conf(self, depends: object = DEFAULT_PACKAGES, *,
                         bundle_media_framework: object = True) -> None:
        """Both Linux consumers' declarations. `None` omits the key entirely,
        which is how an absent declaration differs from a wrong one."""
        deb: dict[str, object] = {}
        if depends is not None:
            deb["depends"] = list(depends) if isinstance(depends, tuple) else depends
        appimage: dict[str, object] = {}
        if bundle_media_framework is not None:
            appimage["bundleMediaFramework"] = bundle_media_framework
        self._write("client/src-tauri/tauri.conf.json", json.dumps(
            {"bundle": {"linux": {"appimage": appimage, "deb": deb}}},
            indent=2) + "\n")

    def write_workflow(self, packages: tuple[str, ...] = DEFAULT_PACKAGES,
                       *, job: str = "build-shell",
                       step: str = APT_STEP,
                       condition: str = "matrix.os == 'linux'",
                       run: str | None = None,
                       preflight_step: str = PREFLIGHT_STEP,
                       preflight_packages: tuple[str, ...] = (),
                       preflight_condition: str | None = None) -> None:
        # The package list is wrapped across a backslash continuation, as the
        # real step's is: tokenizing the halves separately would lose the tail.
        wrapped = " \\\n  ".join(packages)
        # `run` replaces the whole body rather than adding another knob per
        # shape: "installs nothing", "installs only flags" and "installs from a
        # commented-out line" are all one axis -- what the step actually runs.
        body = run if run is not None else (
            "sudo apt-get update\n"
            "sudo apt-get install -y --no-install-recommends \\\n"
            "  libgtk-3-dev librsvg2-dev patchelf \\\n"
            f"  {wrapped}")
        # A second Linux apt step, as the real workflow has: shell-preflight
        # only runs `cargo check`, so requiring runtime plugins there would be
        # wrong. Its presence proves the checker reads the named job's step --
        # and the knobs let a case give it the build job's step name, packages
        # and matrix guard, so the only thing left distinguishing them is which
        # job they sit in.
        preflight_extra = "".join(f" {p}" for p in preflight_packages)
        preflight_guard = ("" if preflight_condition is None
                           else f"\n        if: {preflight_condition}")
        self._write(".github/workflows/shell-release.yml", f"""name: Shell release
on:
  push:
    tags: ['v*']
jobs:
  shell-preflight:
    runs-on: ubuntu-latest
    steps:
      - name: {preflight_step}{preflight_guard}
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends libgtk-3-dev{preflight_extra}
  {job}:
    runs-on: ubuntu-latest
    steps:
      - name: {step}
        if: {condition}
        run: |
{indent(body, 10)}
""")

    def write_verbatim(self, rel: str, body: str) -> None:
        """One target replaced with exact bytes. The typed writers above can
        only produce well-formed files, so the unparseable cases -- and a
        `deb` node that is not an object at all -- have no other way in."""
        self._write(rel, body)

    def delete(self, rel: str) -> None:
        (self.root / rel).unlink()

    def run(self) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            env={**os.environ, "MEDIA_PACKAGING_ROOT": str(self.root)},
            capture_output=True, text=True,
        )


class MediaPluginPackagingTests(unittest.TestCase):
    def tree(self) -> PackagingTree:
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        return PackagingTree(Path(d))

    # --- the accepting arm ------------------------------------------------

    def test_a_complete_tree_passes(self) -> None:
        # Without this a gate that refuses everything would look identical to a
        # gate that works.
        t = self.tree()
        r = t.run()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("media plugin packaging OK", r.stdout)

    def test_a_consumer_with_extra_unrelated_packages_passes(self) -> None:
        # The requirement is coverage, not equality: both consumers legitimately
        # install things REQUIRED_PLUGINS says nothing about.
        t = self.tree()
        t.write_tauri_conf(DEFAULT_PACKAGES + ("libgtk-3-0", "libwebkit2gtk-4.1-0"))
        t.write_workflow(DEFAULT_PACKAGES + ("gstreamer1.0-alsa",))
        r = t.run()
        self.assertEqual(r.returncode, 0, r.stderr)

    # --- the multi-authority direction ------------------------------------

    def test_the_deb_covering_all_does_not_excuse_an_uncovered_apt_step(self) -> None:
        # #8615 itself: the .deb listed every package while the AppImage build
        # runner listed none. A union-based checker passes this tree.
        t = self.tree()
        t.write_workflow(())
        r = t.run()
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("AppImage build runner", r.stderr)
        self.assertNotIn(".deb dependencies", r.stderr)
        for package in DEFAULT_PACKAGES:
            self.assertIn(package, r.stderr)

    def test_a_preflight_step_of_the_same_name_does_not_cover_the_appimage(self) -> None:
        # The job scoping on its own. Both jobs install Linux build deps, so a
        # refactor unifying their step names is entirely plausible -- and a
        # step lookup that scanned every job would then credit shell-preflight's
        # compile-only package set to the AppImage. That is #8615 again, with
        # the gate reporting a clean tree. Name, matrix guard and packages are
        # all the build job's here, so the *only* thing separating the two
        # steps is which job they sit in.
        t = self.tree()
        t.write_workflow((), preflight_step=APT_STEP,
                         preflight_condition="matrix.os == 'linux'",
                         preflight_packages=DEFAULT_PACKAGES)
        r = t.run()
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("AppImage build runner", r.stderr)
        self.assertNotIn(".deb dependencies", r.stderr)
        for package in DEFAULT_PACKAGES:
            self.assertIn(package, r.stderr)

    def test_the_apt_step_covering_all_does_not_excuse_an_uncovered_deb(self) -> None:
        # The mirror direction, so the gate is not accidentally one-sided.
        t = self.tree()
        t.write_tauri_conf(("libgtk-3-0",))
        r = t.run()
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn(".deb dependencies", r.stderr)
        self.assertNotIn("AppImage build runner", r.stderr)
        for package in DEFAULT_PACKAGES:
            self.assertIn(package, r.stderr)

    # --- authority-side degraded reads ------------------------------------

    def test_a_renamed_const_refuses(self) -> None:
        # Silent zero on the authority: no packages required subtracts to no
        # gap, and every consumer passes.
        t = self.tree()
        t.write_media_stack(decl="pub const MEDIA_PLUGINS: &[RequiredPlugin] = &[")
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("REQUIRED_PLUGINS", r.stderr)
        self.assertIn("REFUSED", r.stderr)

    def test_an_empty_required_plugins_refuses(self) -> None:
        # The declaration is present and readable and names nothing. Zero
        # required packages subtracts to no gap at *both* consumers, so the
        # gate would exit 0 announcing "0 package(s) required" -- a gate that
        # inspected nothing, indistinguishable from one that found nothing
        # wrong. That is the property this module is built on, and this is the
        # only refusal reachable without malforming a file.
        t = self.tree()
        t.write_media_stack(())
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("zero plugin entries", r.stderr)
        self.assertIn("REFUSED", r.stderr)

    def test_a_non_literal_package_field_refuses(self) -> None:
        # A partial read must not shrink the authority: three literals read out
        # of four entries is a smaller requirement, not a smaller file.
        t = self.tree()
        t.write_media_stack(trailing=(
            "    RequiredPlugin {\n"
            '        library: "libgstextra.so",\n'
            '        provides: "something",\n'
            "        debian_package: PLUGINS_BASE,\n"
            "    },"))
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("reads inconsistently", r.stderr)

    def test_a_sized_array_declaration_is_accepted(self) -> None:
        # `= [` rather than `= &[`; the constant is the same authority.
        t = self.tree()
        t.write_media_stack(
            decl=f"pub const REQUIRED_PLUGINS: [RequiredPlugin; {len(DEFAULT_PACKAGES)}] = [")
        r = t.run()
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)

    def test_a_type_alias_signature_is_accepted(self) -> None:
        # The type is spelled through an alias; refusing here would be a false
        # refusal on a tree that is entirely correct.
        t = self.tree()
        t.write_media_stack(decl="pub const REQUIRED_PLUGINS: PluginSet = &[")
        r = t.run()
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)

    # --- consumer-side degraded reads -------------------------------------

    def test_a_renamed_build_job_refuses(self) -> None:
        t = self.tree()
        t.write_workflow(job="build-shell-linux")
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("build-shell", r.stderr)
        self.assertIn("REFUSED", r.stderr)

    def test_a_renamed_apt_step_refuses(self) -> None:
        t = self.tree()
        t.write_workflow(step="Install Linux packages")
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn(APT_STEP, r.stderr)

    def test_an_apt_step_no_longer_on_the_linux_arm_refuses(self) -> None:
        # Packages declared on a step that does not run on the AppImage build
        # are not declared, and reading them would report coverage that the
        # bundle never receives.
        t = self.tree()
        t.write_workflow(condition="matrix.os == 'macos'")
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("Linux matrix arm", r.stderr)

    def test_absent_deb_depends_refuses(self) -> None:
        t = self.tree()
        t.write_tauri_conf(None)
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("absent", r.stderr)

    def test_empty_deb_depends_refuses(self) -> None:
        t = self.tree()
        t.write_tauri_conf(())
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("empty", r.stderr)

    def test_bundle_media_framework_not_true_refuses(self) -> None:
        # The precondition under everything this consumer asserts. The apt list
        # is load-bearing only because `bundleMediaFramework` runs
        # linuxdeploy-plugin-gstreamer; off, the AppImage ships no GStreamer at
        # all (issue #6744) and a complete apt list proves nothing. Anything
        # other than exactly `true` is that state, including the key's absence.
        for value, label in ((False, "false"), (None, "absent"), ("true", "a string")):
            with self.subTest(bundle_media_framework=label):
                t = self.tree()
                t.write_tauri_conf(bundle_media_framework=value)
                r = t.run()
                self.assertEqual(r.returncode, 2, r.stdout)
                self.assertIn("bundle.linux.appimage.bundleMediaFramework",
                              r.stderr)
                self.assertIn("REFUSED", r.stderr)

    def test_a_non_list_deb_depends_refuses(self) -> None:
        # A bare string is iterable, so a tolerant reader would "cover" the
        # letters of it. There is no reading of this that is a package list.
        t = self.tree()
        t.write_tauri_conf("gstreamer1.0-libav")
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("not a list", r.stderr)

    def test_a_non_object_deb_node_refuses_for_its_own_reason(self) -> None:
        # `nested` returns `None` both for a missing branch and for one of the
        # wrong shape. Collapsing them is right for the AppImage premise -- not
        # a mapping is not `true` -- but here it would print "bundle.linux.deb
        # .depends is absent; the .deb declares no runtime dependencies at
        # all" over a file whose `deb` key is a string. Same exit code, false
        # sentence: the operator would go looking for a key that is not the
        # problem.
        t = self.tree()
        t.write_verbatim("client/src-tauri/tauri.conf.json", json.dumps(
            {"bundle": {"linux": {"appimage": {"bundleMediaFramework": True},
                                  "deb": "gstreamer1.0-libav"}}},
            indent=2) + "\n")
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("bundle.linux.deb is str, not an object", r.stderr)
        self.assertNotIn("depends is absent", r.stderr)

    def test_a_non_object_bundle_ancestor_is_named_instead_of_the_leaf(self) -> None:
        # Guarding only `deb` moved the false sentence up a level rather than
        # removing it: with `bundle` itself a string, blaming `depends` still
        # sends the operator to a key that is not the problem. The refusal has
        # to name the segment that actually broke the walk.
        t = self.tree()
        t.write_verbatim("client/src-tauri/tauri.conf.json",
                         json.dumps({"bundle": "linux"}, indent=2) + "\n")
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("bundle is str, not an object", r.stderr)
        self.assertNotIn("depends is absent", r.stderr)
        self.assertNotIn("bundle.linux.deb is", r.stderr)

    def test_an_absent_branch_is_not_reported_as_a_shape_problem(self) -> None:
        # `write_tauri_conf(depends=None)` omits `depends` entirely, and the
        # path walk must not describe that as `bundle.linux.deb is NoneType,
        # not an object`. Absence has its own truthful refusal; naming a
        # missing key as mistyped is the same false sentence the walk exists
        # to remove, one level up.
        # The `deb` key is absent outright, so the walk breaks mid-path rather
        # than at the leaf -- `write_tauri_conf(depends=None)` would not reach
        # this, because it still writes a `deb` object.
        t = self.tree()
        t.write_verbatim("client/src-tauri/tauri.conf.json", json.dumps(
            {"bundle": {"linux": {"appimage": {"bundleMediaFramework": True}}}},
            indent=2) + "\n")
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("bundle.linux.deb.depends is absent", r.stderr)
        self.assertNotIn("not an object", r.stderr)

    def test_a_non_object_document_root_is_named_as_the_root(self) -> None:
        # `[1, 2]` is valid JSON, so the walk breaks before consuming any key.
        # An empty path would render as `tauri.conf.json:  is list`, naming
        # nothing.
        t = self.tree()
        t.write_verbatim("client/src-tauri/tauri.conf.json", "[1, 2]\n")
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("the document root is list, not an object", r.stderr)

    def test_a_line_that_only_mentions_an_install_refuses(self) -> None:
        # `echo apt-get install ...` contains the verb but installs nothing.
        # Crediting what follows it counts packages the runner never receives
        # -- fail-open in the one direction this gate must be closed -- so an
        # unrunnable mention is refused rather than guessed at.
        t = self.tree()
        t.write_workflow(run=(
            "sudo apt-get update\n"
            "sudo apt-get install -y --no-install-recommends libgtk-3-dev\n"
            f"echo apt-get install {' '.join(DEFAULT_PACKAGES)}"))
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("without running it", r.stderr)
        for package in DEFAULT_PACKAGES:
            self.assertNotIn(f"  {package}", r.stdout)

    def test_debian_dependency_syntax_names_the_same_package(self) -> None:
        # `bundle.linux.deb.depends` takes real Debian syntax. A version
        # constraint or an alternative still names the package, and a raw
        # string comparison would fail a correct tree while `depends` visibly
        # contains it.
        for shape, entry in (("version constraint", "gstreamer1.0-libav (>= 1.24)"),
                             ("alternative", "gstreamer1.0-libav | gstreamer1.0-plugins-ugly")):
            with self.subTest(shape=shape):
                t = self.tree()
                rest = [p for p in DEFAULT_PACKAGES if p != "gstreamer1.0-libav"]
                t.write_tauri_conf(depends=[entry, *rest])
                r = t.run()
                self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_ordinary_privileged_install_shapes_are_not_refused(self) -> None:
        # A false refusal blocks a correct tree and sends the maintainer to fix
        # a line that is not wrong, so the shapes anyone would plausibly write
        # have to pass. Each installs all four packages; each must exit 0.
        joined = " ".join(DEFAULT_PACKAGES)
        for shape, command in (
            ("sudo -E", f"sudo -E apt-get install -y {joined}"),
            ("sudo --preserve-env", f"sudo --preserve-env apt-get install -y {joined}"),
            ("assignment after sudo",
             f"sudo DEBIAN_FRONTEND=noninteractive apt-get install -y {joined}"),
            ("assignment before sudo",
             f"DEBIAN_FRONTEND=noninteractive sudo apt-get install -y {joined}"),
            ("flag before the verb", f"sudo apt-get -y install {joined}"),
            ("apt rather than apt-get", f"sudo apt install -y {joined}"),
            ("no sudo at all", f"apt-get install -y {joined}"),
        ):
            with self.subTest(shape=shape):
                t = self.tree()
                t.write_workflow(run=f"sudo apt-get update\n{command}")
                r = t.run()
                self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_an_install_inside_shell_control_flow_refuses(self) -> None:
        # The head-check only sees one line. An install guarded by a condition
        # that does not hold on the release runner has an `apt-get` head token
        # on its own line and would otherwise be credited in full -- #8615 with
        # a green check. The gate cannot tell which lines run, so it refuses.
        t = self.tree()
        t.write_workflow(run=(
            "sudo apt-get update\n"
            "sudo apt-get install -y libgtk-3-dev\n"
            "if false; then\n"
            f"  sudo apt-get install -y {' '.join(DEFAULT_PACKAGES)}\n"
            "fi"))
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("uses shell control flow", r.stderr)
        for package in DEFAULT_PACKAGES:
            self.assertNotIn(f"  {package}", r.stdout)

    def test_a_multiarch_qualifier_names_the_same_package(self) -> None:
        # `gstreamer1.0-libav:any` is valid syntax for the same package; a raw
        # comparison would report it missing while the file visibly contains
        # it. Both consumers must normalize it, not just one -- `DEBIAN_NAME`
        # admits the qualifier on the apt side, so storing the token unchanged
        # there would reopen the false gap on the other consumer.
        rest = [q for q in DEFAULT_PACKAGES if q != "gstreamer1.0-libav"]
        with self.subTest(consumer=".deb"):
            t = self.tree()
            t.write_tauri_conf(depends=["gstreamer1.0-libav:any", *rest])
            r = t.run()
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        with self.subTest(consumer="apt step"):
            t = self.tree()
            t.write_workflow(run=(
                "sudo apt-get update\nsudo apt-get install -y "
                + " ".join(f"{q}:amd64" for q in DEFAULT_PACKAGES)))
            r = t.run()
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_an_operator_ends_the_install_argument_list(self) -> None:
        # A tolerant or chained install is a shape a maintainer could write.
        # Treating `||` as a package candidate refuses it with a message
        # blaming a "shell or workflow expression" the line does not contain.
        joined = " ".join(DEFAULT_PACKAGES)
        for shape, command in (
            ("tolerant", f"sudo apt-get install -y {joined} || true"),
            ("chained", f"sudo apt-get install -y {joined} && echo ok"),
        ):
            with self.subTest(shape=shape):
                t = self.tree()
                t.write_workflow(run=f"sudo apt-get update\n{command}")
                r = t.run()
                self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_a_malformed_depends_entry_refuses_rather_than_reading_as_a_gap(self) -> None:
        # Stringifying a non-package entry would produce a junk name, and the
        # tree would surface as a coverage gap (exit 1) rather than a file this
        # gate cannot read (exit 2), against the module's exit-code contract.
        for shape, entry in (("object", {"name": "gstreamer1.0-libav"}),
                             ("number", 7),
                             ("empty name", "   ")):
            with self.subTest(shape=shape):
                t = self.tree()
                t.write_tauri_conf(depends=[entry, *DEFAULT_PACKAGES])
                r = t.run()
                self.assertEqual(r.returncode, 2, r.stdout)
                self.assertIn("bundle.linux.deb.depends contains", r.stderr)

    def test_an_unexpanded_expression_refuses_rather_than_reading_as_a_gap(self) -> None:
        # Hoisting the shared list into an `env` value is a plausible refactor:
        # this workflow already has two Linux apt steps with overlapping lists.
        # Crediting `$PACKAGES` as a literal name would report the tree as
        # missing all four while the step installs them -- blocking CI and
        # telling the maintainer to add what is already there.
        for shape, tail in (("shell variable", "$PACKAGES"),
                            ("workflow expression", "${{ env.LINUX_APT_PACKAGES }}")):
            with self.subTest(shape=shape):
                t = self.tree()
                t.write_workflow(run=("sudo apt-get update\n"
                                      f"sudo apt-get install -y {tail}"))
                r = t.run()
                self.assertEqual(r.returncode, 2, r.stdout)
                self.assertIn("not a literal package name", r.stderr)
                self.assertNotIn("is missing", r.stderr)

    def test_a_heredoc_body_is_not_credited_as_an_install(self) -> None:
        # A package written into a file is not a package installed. The body
        # line has an `apt-get` head token and no control-flow keyword, so
        # nothing else in this gate would tell it apart from a real install.
        t = self.tree()
        t.write_workflow(run=(
            "sudo apt-get update\n"
            "sudo apt-get install -y libgtk-3-dev\n"
            "cat <<'EOF' > /tmp/later.sh\n"
            f"sudo apt-get install -y {' '.join(DEFAULT_PACKAGES)}\n"
            "EOF"))
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("writes a heredoc", r.stderr)

    def test_ordinary_prose_in_the_step_is_not_control_flow(self) -> None:
        # `shlex` strips quotes, so a whole-token keyword match refuses
        # `echo "done"` and any sentence carrying a bare `for` -- a false
        # refusal on a tree that is entirely correct.
        for shape, extra in (
            ("quoted keyword", 'echo "done"'),
            ("keyword in prose", "echo Installed GStreamer plugins for the AppImage"),
            ("keyword as a package-ish word", "echo done building"),
        ):
            with self.subTest(shape=shape):
                t = self.tree()
                t.write_workflow(run=("sudo apt-get update\n"
                                      "sudo apt-get install -y --no-install-recommends "
                                      f"{' '.join(DEFAULT_PACKAGES)}\n{extra}"))
                r = t.run()
                self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_a_later_chained_command_cannot_supply_packages(self) -> None:
        # Reported by CodeRabbit on #8630 and reproduced: judging the line as a
        # whole let the FIRST command (`apt-get update`) satisfy the "is this
        # really an apt install" check, while `tokens.index("install")` then
        # found the SECOND command's verb and credited its arguments. A step
        # installing nothing read as full coverage -- the exact fail-open this
        # gate exists to prevent, so each command is judged on its own.
        joined = " ".join(DEFAULT_PACKAGES)
        for shape, command in (
            ("update && echo", f"sudo apt-get update && echo apt-get install -y {joined}"),
            ("real install, then echo",
             f"sudo apt-get install -y libgtk-3-dev && echo apt-get install {joined}"),
            ("semicolon separated",
             f"sudo apt-get update; echo apt-get install -y {joined}"),
        ):
            with self.subTest(shape=shape):
                t = self.tree()
                t.write_workflow(run=command)
                r = t.run()
                self.assertNotEqual(r.returncode, 0, r.stdout)
                self.assertIn("without running it", r.stderr)

    def test_an_apt_step_that_installs_nothing_refuses(self) -> None:
        # The step still exists, on the right job and the right arm, and runs
        # no `apt-get install`. Zero packages read out of it is not zero
        # packages installed by it -- it is a step this gate cannot read.
        t = self.tree()
        t.write_workflow(run="sudo apt-get update\n"
                             "echo no packages are installed here")
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("runs no", r.stderr)
        self.assertIn("REFUSED", r.stderr)

    def test_an_install_line_of_only_flags_refuses(self) -> None:
        # `install -y --no-install-recommends` with the package list dropped:
        # every token is a flag, so the covered set is empty and would subtract
        # to no gap. Reached only with a body that installs nothing else, which
        # is why the #8615 fixture (still installing libgtk-3-dev and friends)
        # does not exercise this branch.
        t = self.tree()
        t.write_workflow(run="sudo apt-get update\n"
                             "sudo apt-get install -y --no-install-recommends")
        r = t.run()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("tokenizes to zero package names", r.stderr)

    def test_a_commented_out_install_line_covers_nothing(self) -> None:
        # `tokens.index("install")` drops everything before the verb, `#`
        # included, so a substring match on the line would let a disabled
        # install declare packages the runner never receives -- fail-open in
        # the one direction this gate exists to close.
        t = self.tree()
        t.write_workflow(run=(
            "sudo apt-get update\n"
            "sudo apt-get install -y --no-install-recommends libgtk-3-dev\n"
            f"# sudo apt-get install -y {' '.join(DEFAULT_PACKAGES)}"
            "  # TODO: re-enable"))
        r = t.run()
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("AppImage build runner", r.stderr)
        for package in DEFAULT_PACKAGES:
            self.assertIn(package, r.stderr)

    def test_a_comment_after_a_live_install_covers_nothing(self) -> None:
        # The leading-`#` filter's blind spot, in both shapes that reach it.
        # `shlex.split` tokenizes a comment as packages unless told not to, and
        # `tokens.index("install")` then drops the `#` with everything before
        # the verb -- so a disabled install riding on a live line would declare
        # the whole set. The continuation shape gets there without a `#` first
        # on any *source* line at all: the backslash join folds the commented
        # tail onto the live install.
        for label, run in (
            ("trailing comment",
             "sudo apt-get update\n"
             "sudo apt-get install -y --no-install-recommends libgtk-3-dev"
             f"  # TODO: restore apt-get install {' '.join(DEFAULT_PACKAGES)}"),
            ("commented continuation",
             "sudo apt-get update\n"
             "sudo apt-get install -y --no-install-recommends libgtk-3-dev \\\n"
             f"  # {' '.join(DEFAULT_PACKAGES)}"),
        ):
            with self.subTest(shape=label):
                t = self.tree()
                t.write_workflow(run=run)
                r = t.run()
                self.assertEqual(r.returncode, 1, r.stdout)
                self.assertIn("AppImage build runner", r.stderr)
                self.assertNotIn(".deb dependencies", r.stderr)
                for package in DEFAULT_PACKAGES:
                    self.assertIn(package, r.stderr)

    # --- harness integrity ------------------------------------------------

    def test_the_harness_reads_the_fixture_not_the_real_tree(self) -> None:
        # If MEDIA_PACKAGING_ROOT were ignored, every case above would be
        # measuring this checkout and the passing ones would be vacuous.
        self.assertNotIn(SENTINEL, REAL_MEDIA_STACK.read_text(encoding="utf-8"),
                         "the sentinel must not exist in the real tree, or this "
                         "test proves nothing")
        t = self.tree()
        t.write_media_stack(DEFAULT_PACKAGES + (SENTINEL,))
        r = t.run()
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn(SENTINEL, r.stderr)

    def test_a_fixture_missing_any_target_file_refuses(self) -> None:
        # Each target deleted in turn. A missing file must never read as a
        # consumer with nothing to cover.
        messages: list[str] = []
        for rel, marker in (
            ("client/src-tauri/src/media_stack.rs", "package authority"),
            ("client/src-tauri/tauri.conf.json", ".deb consumer"),
            (".github/workflows/shell-release.yml", "AppImage consumer"),
        ):
            with self.subTest(deleted=rel):
                t = self.tree()
                t.delete(rel)
                r = t.run()
                self.assertEqual(r.returncode, 2, r.stdout)
                self.assertIn("does not exist", r.stderr)
                self.assertIn(marker, r.stderr)
                messages.append(r.stderr)
        self.assertEqual(len(set(messages)), 3,
                         "each missing target must be distinguishable")

    def test_the_refusals_are_pairwise_distinguishable(self) -> None:
        # No refusal may hide behind another: an operator has to learn which
        # precondition they tripped, and no test may satisfy its assertion on
        # the wrong refusal.
        cases: dict[str, subprocess.CompletedProcess] = {}

        t = self.tree(); t.write_media_stack(decl="pub const MEDIA_PLUGINS: &[RequiredPlugin] = &[")
        cases["renamed const"] = t.run()

        t = self.tree(); t.write_media_stack(())
        cases["empty authority"] = t.run()

        t = self.tree(); t.write_media_stack(trailing=(
            "    RequiredPlugin {\n"
            '        library: "libgstextra.so",\n'
            '        provides: "something",\n'
            "        debian_package: PLUGINS_BASE,\n"
            "    },"))
        cases["non-literal field"] = t.run()

        t = self.tree(); t.write_workflow(job="build-shell-linux")
        cases["renamed job"] = t.run()

        t = self.tree(); t.write_workflow(step="Install Linux packages")
        cases["renamed step"] = t.run()

        t = self.tree(); t.write_workflow(condition="matrix.os == 'macos'")
        cases["wrong arm"] = t.run()

        t = self.tree(); t.write_tauri_conf(None)
        cases["absent depends"] = t.run()

        t = self.tree(); t.write_tauri_conf(())
        cases["empty depends"] = t.run()

        t = self.tree(); t.write_tauri_conf("gstreamer1.0-libav")
        cases["non-list depends"] = t.run()

        t = self.tree(); t.write_tauri_conf(bundle_media_framework=False)
        cases["media framework off"] = t.run()

        t = self.tree(); t.write_workflow(run="sudo apt-get update\n"
                                              "echo no packages are installed here")
        cases["no install line"] = t.run()

        t = self.tree(); t.write_workflow(
            run="sudo apt-get update\n"
                "sudo apt-get install -y --no-install-recommends")
        cases["flags-only install"] = t.run()

        t = self.tree(); t.write_verbatim("client/src-tauri/tauri.conf.json",
                                          json.dumps({"bundle": {"linux": {
                                              "appimage": {"bundleMediaFramework": True},
                                              "deb": "gstreamer1.0-libav"}}}))
        cases["non-object deb node"] = t.run()

        t = self.tree(); t.write_verbatim("client/src-tauri/tauri.conf.json",
                                          json.dumps({"bundle": "linux"}))
        cases["non-object bundle ancestor"] = t.run()

        t = self.tree(); t.write_verbatim("client/src-tauri/tauri.conf.json",
                                          "[1, 2]")
        cases["non-object document root"] = t.run()

        t = self.tree(); t.write_workflow(
            run="sudo apt-get update\necho apt-get install libgtk-3-dev")
        cases["mention without a command"] = t.run()

        t = self.tree(); t.write_workflow(
            run="sudo apt-get update\nif true; then sudo apt-get install -y x\nfi")
        cases["shell control flow"] = t.run()

        t = self.tree(); t.write_workflow(
            run="sudo apt-get update\nsudo apt-get install -y $PACKAGES")
        cases["unexpanded expression"] = t.run()

        t = self.tree(); t.write_workflow(
            run="sudo apt-get update\ncat <<EOF > /tmp/x\nsudo apt-get install -y a\nEOF")
        cases["heredoc"] = t.run()

        t = self.tree(); t.write_tauri_conf(depends=[7, *DEFAULT_PACKAGES])
        cases["non-string depends entry"] = t.run()

        t = self.tree(); t.write_tauri_conf(depends=["  ", *DEFAULT_PACKAGES])
        cases["empty depends entry"] = t.run()

        # The three refusals reachable only by malforming a file. Each already
        # degrades into some other refusal rather than a pass, so they are here
        # for the distinguishability invariant, not to close a fail-open.
        t = self.tree(); t.write_verbatim("client/src-tauri/tauri.conf.json",
                                          "{not json at all")
        cases["unparseable json"] = t.run()

        t = self.tree(); t.write_verbatim(".github/workflows/shell-release.yml",
                                          "jobs: [unclosed\n")
        cases["unparseable yaml"] = t.run()

        t = self.tree(); t.write_workflow(
            run='sudo apt-get update\n'
                'sudo apt-get install -y "libgtk-3-dev')
        cases["untokenizable install"] = t.run()

        for name, result in cases.items():
            with self.subTest(case=name):
                self.assertEqual(result.returncode, 2, result.stdout)
        self.assertEqual(len(set(r.stderr for r in cases.values())), len(cases),
                         "two refusals share a message")


if __name__ == "__main__":
    unittest.main()
