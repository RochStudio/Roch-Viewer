"""Where the low-level driver is looked for.

It is not distributed with this project, so it is wherever the person running
the tool put it -- and that is a different directory depending on whether the
tool is frozen. Getting this wrong is not a subtle failure: the app refuses to
start and names a path nobody has ever written to.
"""

import os
import sys
import unittest
from unittest import mock

import driver_path


class SearchOrderTest(unittest.TestCase):
    def test_from_source_it_looks_beside_the_modules(self):
        with mock.patch.object(sys, "frozen", False, create=True):
            directories = driver_path.search_directories()
        here = os.path.dirname(os.path.realpath(driver_path.__file__))
        self.assertIn(here, directories)

    def test_frozen_it_looks_beside_the_executable_first(self):
        # The directory a user can see and drop a file into. Anything else
        # first would find a stale copy in preference to theirs.
        with mock.patch.object(sys, "frozen", True, create=True), \
                mock.patch.object(sys, "executable", r"C:\apps\RochViewer.exe"):
            directories = driver_path.search_directories()
        self.assertEqual(directories[0], r"C:\apps")

    def test_the_unpacked_directory_is_searched_last(self):
        # This is where a bundled copy lands. Searching it first is what the
        # original did, and it broke the moment the driver stopped being
        # bundled -- it reported the DLL missing from a temp directory that
        # had never held one.
        with mock.patch.object(sys, "frozen", True, create=True), \
                mock.patch.object(sys, "executable", r"C:\apps\RochViewer.exe"), \
                mock.patch.object(sys, "_MEIPASS", r"C:\temp\_MEI1234",
                                  create=True):
            directories = driver_path.search_directories()
        self.assertEqual(directories[-1], r"C:\temp\_MEI1234")
        self.assertLess(directories.index(r"C:\apps"),
                        directories.index(r"C:\temp\_MEI1234"))

    def test_no_directory_is_searched_twice(self):
        with mock.patch.object(sys, "frozen", True, create=True), \
                mock.patch.object(sys, "executable",
                                  os.path.join(os.path.dirname(
                                      os.path.realpath(driver_path.__file__)),
                                      "RochViewer.exe")):
            directories = driver_path.search_directories()
        self.assertEqual(len(directories), len(set(directories)))


class MissingDriverTest(unittest.TestCase):
    def test_it_reports_nothing_rather_than_a_path_that_does_not_exist(self):
        with mock.patch.object(driver_path, "search_directories",
                               return_value=[r"C:\nowhere"]):
            self.assertIsNone(driver_path.find_driver())

    def test_the_message_says_what_to_do_and_where(self):
        # Someone hitting this has a working download and nowhere to put it.
        with mock.patch.object(driver_path, "search_directories",
                               return_value=[r"C:\apps", r"C:\src"]):
            message = driver_path.missing_message()
        self.assertIn("not distributed", message)
        self.assertIn("highrez.co.uk", message)
        self.assertIn(r"C:\apps", message)
        self.assertIn(r"C:\src", message)

    def test_it_finds_a_driver_that_is_there(self):
        import tempfile

        with tempfile.TemporaryDirectory() as folder:
            path = os.path.join(folder, driver_path.DLL_NAME)
            open(path, "wb").close()
            with mock.patch.object(driver_path, "search_directories",
                                   return_value=[folder]):
                self.assertEqual(driver_path.find_driver(), path)


class NotBundledTest(unittest.TestCase):
    def test_the_spec_does_not_ship_the_driver(self):
        # The data entry, not the prose: the spec explains at length why the
        # driver is absent, and matching raw text finds that explanation.
        spec = open("RochViewer.spec", encoding="utf-8").read()
        for name in ("inpoutx64.dll", "inpoutx64.sys"):
            with self.subTest(name=name):
                self.assertNotIn("('%s'" % name, spec)
                self.assertNotIn('("%s"' % name, spec)
        # And the one file that is bundled still is, so this cannot pass by
        # the datas list having quietly emptied.
        self.assertIn("('icon.ico', '.')", spec)

    def test_the_driver_is_not_tracked(self):
        ignored = open(".gitignore", encoding="utf-8").read()
        self.assertIn("inpoutx64.dll", ignored)
        self.assertIn("inpoutx64.sys", ignored)


if __name__ == "__main__":
    unittest.main()
