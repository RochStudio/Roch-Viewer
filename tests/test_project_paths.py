# Roch Viewer -- a read-only memory-controller and timing viewer.
# Copyright (C) 2026 Roch Studio
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.


"""That things shipped with the project can still be found from a module.

Every lookup here was once "beside my own module", which was the project
root until the modules moved into packages. Afterwards none of them found
anything, and not one of them raised:

    the driver   find_driver() returned None, so every register-backed
                 reading became N/A while the window still opened and
                 looked healthy
    the icon     the taskbar fell back to Tk's default, while the EXE's
                 file icon -- baked in at build time -- stayed correct
    the launcher elevation relaunched a module that cannot run as a
                 script, so the UAC prompt appeared and nothing followed

They are grouped in one file because they share a cause: a module cannot
assume the project root is its own directory.
"""

import os
import sys
import unittest
from unittest import mock

from rochviewer import paths
from rochviewer.hardware import driver_path
from rochviewer.ui import asset_path


def project_root():
    """The root of this checkout, found without using the code under test."""
    return os.path.dirname(os.path.dirname(os.path.realpath(__file__)))


class TheProjectRootIsReachableTest(unittest.TestCase):
    """The single assertion the package reorganisation needed."""

    def test_the_driver_is_looked_for_at_the_project_root(self):
        # Not "the driver is found" -- it is a third-party download, is
        # gitignored, and is absent on CI. What must hold is that the
        # directory the README tells people to use is searched.
        self.assertIn(project_root(), driver_path.search_directories())

    def test_the_icon_is_looked_for_at_the_project_root(self):
        self.assertIn(project_root(), asset_path.search_directories())

    def test_the_launcher_is_found(self):
        from rochviewer.ui import main
        self.assertEqual(
            main.launcher_path(),
            os.path.join(project_root(), main.LAUNCHER_NAME),
        )

    def test_the_launcher_is_not_this_module(self):
        # Relaunching main.py is the bug being pinned: absolute imports mean
        # it exits with ModuleNotFoundError before drawing anything.
        from rochviewer.ui import main
        self.assertNotEqual(
            os.path.realpath(main.launcher_path()),
            os.path.realpath(main.__file__),
        )


class ElevationInterpreterTest(unittest.TestCase):
    """Elevation decides which interpreter the user ends up with.

    It starts a brand new process, so a console interpreter named here leaves
    a console window sitting behind the viewer for the rest of the session --
    one the user never asked for, unlike the shell they started from.

    Forward slashes below because ntpath splits on either, and a Windows
    literal in a test is a backslash-escape accident waiting to happen.
    """

    WINDOWED = "C:/Py/pythonw.exe"

    def test_a_windowed_interpreter_is_kept_as_it_is(self):
        from rochviewer.ui import main
        self.assertEqual(main.windowed_interpreter(self.WINDOWED),
                         self.WINDOWED)

    def test_it_falls_back_when_there_is_no_windowed_one(self):
        # An embedded or repackaged distribution may not ship pythonw, and a
        # console is worth much less than not starting.
        from rochviewer.ui import main
        missing = "C:/no-such-dir-here/python.exe"
        self.assertEqual(main.windowed_interpreter(missing), missing)

    def test_it_prefers_a_windowed_interpreter_that_is_there(self):
        from rochviewer.ui import main
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            open(os.path.join(directory, "pythonw.exe"), "wb").close()
            chosen = main.windowed_interpreter(
                os.path.join(directory, "python.exe"))
        self.assertTrue(chosen.lower().endswith("pythonw.exe"))

    def _elevate(self, frozen, executable):
        """Run run_as_admin against a fake shell and return its arguments."""
        from rochviewer.ui import main
        calls = []

        class FakeShell:
            @staticmethod
            def ShellExecuteW(handle, verb, program, parameters, cwd, show):
                calls.append((verb, program, parameters))

        with mock.patch.object(main.ctypes, "windll") as windll:
            with mock.patch.object(main.sys, "frozen", frozen, create=True):
                with mock.patch.object(main.sys, "executable", executable):
                    with mock.patch.object(main, "windowed_interpreter",
                                           lambda: self.WINDOWED):
                        with mock.patch.object(
                                main, "launcher_path",
                                lambda: "C:/app/run_viewer.py"):
                            with mock.patch.object(main.sys, "exit",
                                                   lambda code=0: None):
                                windll.shell32 = FakeShell
                                main.run_as_admin()
        return calls

    def test_the_chosen_interpreter_is_what_elevation_actually_runs(self):
        # The helper returning the right name proves nothing on its own --
        # run_as_admin has to hand it to ShellExecuteW. This is the assertion
        # that catches the value being computed and then ignored, which is
        # how the read width was lost a few commits ago.
        calls = self._elevate(False, "C:/Py/python.exe")
        self.assertEqual(len(calls), 1)
        verb, program, parameters = calls[0]
        self.assertEqual(verb, "runas")
        self.assertEqual(program, self.WINDOWED)
        self.assertIn("run_viewer.py", parameters)

    def test_a_frozen_build_elevates_into_itself(self):
        # It is already a windowed executable, and handing an interpreter the
        # EXE path would elevate into something that cannot run it.
        calls = self._elevate(True, "C:/app/RochViewer.exe")
        self.assertEqual(calls, [("runas", "C:/app/RochViewer.exe", None)])


class ModuleChainTest(unittest.TestCase):
    def test_it_reaches_the_root_from_a_two_deep_module(self):
        from rochviewer.ui import main
        self.assertIn(project_root(), paths.module_chain(main.__file__))

    def test_it_reaches_the_root_from_a_one_deep_module(self):
        self.assertIn(project_root(), paths.module_chain(paths.__file__))

    def test_it_starts_at_the_module_and_walks_outward(self):
        chain = paths.module_chain(asset_path.__file__)
        for nearer, further in zip(chain, chain[1:]):
            self.assertEqual(os.path.dirname(nearer), further)

    def test_it_stops_at_the_drive_root(self):
        chain = paths.module_chain(os.path.abspath(os.sep) + "x.py", levels=9)
        self.assertEqual(len(chain), 1)


class SearchOrderTest(unittest.TestCase):
    def test_the_driver_is_looked_for_beside_the_executable_first(self):
        # The directory a user can see and drop a file into.
        with mock.patch.object(sys, "frozen", True, create=True), \
                mock.patch.object(sys, "executable", r"C:\apps\RochViewer.exe"):
            directories = driver_path.search_directories()
        self.assertEqual(directories[0], r"C:\apps")

    def test_a_bundled_driver_ranks_last(self):
        # The user's own copy must win over one that happens to be bundled.
        with mock.patch.object(sys, "frozen", True, create=True), \
                mock.patch.object(sys, "executable", r"C:\apps\RochViewer.exe"), \
                mock.patch.object(sys, "_MEIPASS", r"C:\temp\_MEI1", create=True):
            directories = driver_path.search_directories()
        self.assertEqual(directories[-1], r"C:\temp\_MEI1")

    def test_a_bundled_icon_ranks_early(self):
        # Opposite of the driver, deliberately: the icon is ours to ship, so
        # the bundled copy is the correct one.
        with mock.patch.object(sys, "frozen", True, create=True), \
                mock.patch.object(sys, "executable", r"C:\apps\RochViewer.exe"), \
                mock.patch.object(sys, "_MEIPASS", r"C:\temp\_MEI1", create=True):
            directories = asset_path.search_directories()
        self.assertEqual(directories[1], r"C:\temp\_MEI1")

    def test_no_directory_is_searched_twice(self):
        for directories in (driver_path.search_directories(),
                            asset_path.search_directories()):
            self.assertEqual(len(directories), len(set(directories)))


class MissingIsNotFatalTest(unittest.TestCase):
    def test_a_missing_driver_is_none_not_a_raise(self):
        self.assertIsNone(driver_path.find_driver("no-such-driver.dll"))

    def test_a_missing_asset_is_none_not_a_raise(self):
        self.assertIsNone(asset_path.find_asset("no-such-asset.bin"))

    def test_the_missing_message_names_every_directory_searched(self):
        message = driver_path.missing_message()
        for directory in driver_path.search_directories():
            self.assertIn(directory, message)


if __name__ == "__main__":
    unittest.main()
