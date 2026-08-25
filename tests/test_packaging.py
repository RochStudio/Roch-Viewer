"""The spec has to name every module PyInstaller cannot find for itself.

The first version of this file tested the wrong rule. It assumed PyInstaller
misses any import written inside a function body, and it would have failed
that way round even if the code had worked -- which it did not, because the
static and lazy scans both used ``ast.walk`` and so reported the same names,
making the assertion pass no matter what.

What is actually true was settled against the shipped v74 EXE. PyInstaller
scans bytecode for import opcodes, and those appear at any nesting depth, so
a plain ``import x`` inside a function is found. v74 carries six modules --
asus_ec, ite_superio, nct679x, intel_rapl, ddr4_spd and nvidia_gpu -- that
the spec has never named, and they are in its archive all the same.

The blind spot is ``__import__`` with a *variable*, where the name exists
only at runtime and there is no opcode to read it from. ``am5_profile`` does
exactly that in ``_import_call`` and ``_offsets_gate``, which is how five
hardware transports are reached. Those are the names that must be listed, and
that is what this file checks.
"""

from __future__ import annotations

import ast
import io
import os
import re
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPEC = os.path.join(ROOT, "RochViewer.spec")

# The helpers that import by variable. Anything routed through these is
# invisible to the build and has to be named in the spec.
DYNAMIC_HELPERS = ("_import_call", "_offsets_gate")

# Bench tools, excluded on the name so the rule stays one rule. A probe or a
# logger is run from source against hardware and has no business in the EXE.
BENCH_SUFFIXES = ("_probe", "_logger")

ENTRY_POINTS = frozenset({"main", "am5_probe"})


def local_modules():
    """Every module in the project root, by import name."""
    return {name[:-3] for name in os.listdir(ROOT)
            if name.endswith(".py") and not name.startswith("_")}


def shipped_modules():
    """The modules that can end up in the viewer build."""
    return {name for name in local_modules()
            if not name.endswith(BENCH_SUFFIXES) and name not in ENTRY_POINTS}


def _parse(name):
    with io.open(os.path.join(ROOT, name + ".py"), encoding="utf-8") as handle:
        return ast.parse(handle.read())


def dynamically_imported():
    """Modules named as string arguments to the import-by-variable helpers."""
    found = set()
    for name in sorted(shipped_modules() | {"main"}):
        for node in ast.walk(_parse(name)):
            if not isinstance(node, ast.Call):
                continue
            called = getattr(node.func, "id", None)
            if called in DYNAMIC_HELPERS and node.args:
                first = node.args[0]
                if isinstance(first, ast.Constant) and isinstance(first.value,
                                                                  str):
                    found.add(first.value)
    return found


def dynamic_call_sites():
    """Every ``__import__`` call, so a new one cannot appear unnoticed."""
    sites = []
    for name in sorted(shipped_modules() | {"main"}):
        for node in ast.walk(_parse(name)):
            if isinstance(node, ast.Call) and \
                    getattr(node.func, "id", None) == "__import__":
                literal = (node.args and isinstance(node.args[0], ast.Constant))
                sites.append((name, node.lineno, bool(literal)))
    return sites


def spec_hidden_imports():
    """The names listed in the spec's hiddenimports block."""
    with io.open(SPEC, encoding="utf-8") as handle:
        text = handle.read()
    block = text.split("hiddenimports = [", 1)[1].split("]", 1)[0]
    # Quoted names only, so the comments in that block cannot fake a match.
    return set(re.findall(r"['\"]([A-Za-z_][A-Za-z0-9_.]*)['\"]", block))


class DynamicImportTest(unittest.TestCase):
    def test_nothing_imports_by_variable(self):
        # The check below is vacuous while this holds, and that is the point.
        # Import-by-variable is the one thing PyInstaller cannot follow: it
        # scans bytecode, so a plain import inside a function body is found,
        # but __import__(name) with a variable is not, and the module goes
        # missing from the build with nothing to say so.
        #
        # The helpers that did this belonged to the AMD profile and are not
        # in this tree. If the pattern comes back, this fails and
        # test_every_dynamically_imported_module_is_named starts doing work.
        self.assertEqual(
            dynamically_imported(), set(),
            "something imports by variable again; PyInstaller cannot see "
            "those, so every one has to be named in the spec by hand")

    def test_no_import_call_takes_a_variable(self):
        # The same rule at the __import__ call itself, which is what a future
        # helper would be built on.
        for module, line, literal in dynamic_call_sites():
            with self.subTest(module=module, line=line):
                self.assertTrue(
                    literal,
                    "%s:%d calls __import__ with a variable" % (module, line))

    def test_every_dynamically_imported_module_is_named(self):
        missing = sorted(name for name in dynamically_imported()
                         if name not in spec_hidden_imports())
        self.assertEqual(
            missing, [],
            "reached only through __import__ with a variable, which "
            "PyInstaller cannot follow, so the packaged build would drop "
            "them: %s. Add them to hiddenimports in RochViewer.spec."
            % ", ".join(missing)
        )

    def test_no_dynamic_import_escapes_the_helpers(self):
        # A bare __import__(variable) somewhere else would be invisible to
        # the check above as well as to PyInstaller.
        loose = [(name, line) for name, line, literal in dynamic_call_sites()
                 if not literal and name != "am5_profile"]
        self.assertEqual(loose, [],
                         "__import__ by variable outside am5_profile's two "
                         "helpers: %s" % loose)


class SpecHygieneTest(unittest.TestCase):
    def test_the_spec_block_parses(self):
        # If this list ever stops being readable the whole check quietly
        # passes, which is worse than failing.
        self.assertIn("customtkinter", spec_hidden_imports())

    def test_no_bench_tool_is_packaged(self):
        # A probe writes to hardware in ways the viewer never does; shipping
        # one puts that code in every user's EXE.
        bench = sorted(name for name in spec_hidden_imports() & local_modules()
                       if name.endswith(BENCH_SUFFIXES))
        self.assertEqual(bench, [], "bench tools must not ship: %s"
                         % ", ".join(bench))

    def test_the_spec_names_no_module_that_no_longer_exists(self):
        # A stale name is a silent no-op that makes the list look maintained.
        # Third-party packages are not local modules, so only check names
        # that look like this project's own.
        third_party = {"customtkinter", "wmi", "win32com", "pythoncom",
                       "pywintypes"}
        stale = sorted(name for name in spec_hidden_imports()
                       if name not in local_modules()
                       and name.split(".")[0] not in third_party)
        self.assertEqual(stale, [])


if __name__ == "__main__":
    unittest.main()
