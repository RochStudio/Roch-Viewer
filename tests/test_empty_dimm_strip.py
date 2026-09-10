"""An absent DIMM channel must not reserve CTkFrame's default 200px."""
import types
import unittest
from unittest.mock import patch

from rochviewer.ui.main import TimingGUI


class EmptyDimmStripTest(unittest.TestCase):
    def test_empty_channel_creates_no_placeholder(self):
        with patch("rochviewer.ui.main.ctk.CTkFrame") as frame:
            TimingGUI._dimm_strip_column(types.SimpleNamespace(), [], 0)
            TimingGUI._dimm_strip_column(types.SimpleNamespace(), [], 1)
        frame.assert_not_called()

