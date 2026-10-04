"""Display selection/rollback tests with mocked XrandR; no display changes."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True

SCRIPT = Path(__file__).resolve().parents[2] / "src-tauri/scripts/change_display_resolution.py"
spec = importlib.util.spec_from_file_location("display_tool", SCRIPT)
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)
SAMPLE = """Screen 0: minimum 8 x 8, current 1920 x 1080, maximum 32767 x 32767
HDMI-0 connected primary 1920x1080+0+0
   1920x1080     60.00*+  120.00
   2560x1440     59.95
DP-0 disconnected
   1024x768      60.00
DP-1 connected 1280x720+1920+0
   1280x720      60.00*
"""


class DisplayTests(unittest.TestCase):
    def test_parse_connected_outputs_and_current_refresh(self):
        outputs = tool.parse_outputs(SAMPLE)
        self.assertEqual(set(outputs), {"HDMI-0", "DP-1"})
        self.assertEqual(outputs["HDMI-0"]["current"], ("1920x1080", "60.00"))
        self.assertIn(("2560x1440", "59.95"), outputs["HDMI-0"]["modes"])

    def test_unsupported_mode_never_calls_xrandr(self):
        with patch.object(tool, "apply") as apply:
            with self.assertRaises(ValueError):
                tool.change("HDMI-0", "9999x9999", "60.00", tool.parse_outputs(SAMPLE))
            apply.assert_not_called()

    def test_confirm_keeps_mode_and_cancels_watchdog(self):
        with patch.object(tool.subprocess, "Popen") as watchdog, patch.object(tool, "apply") as apply, patch.object(tool, "confirm", return_value=True):
            tool.change("HDMI-0", "2560x1440", "59.95", tool.parse_outputs(SAMPLE))
            apply.assert_called_once_with("HDMI-0", "2560x1440", "59.95")
            token = watchdog.call_args.args[0][3]
            self.assertFalse(Path(token).exists())
            self.assertTrue(watchdog.call_args.kwargs["start_new_session"])

    def test_rejection_restores_previous_mode(self):
        with patch.object(tool.subprocess, "Popen"), patch.object(tool, "apply") as apply, patch.object(tool, "confirm", return_value=False):
            tool.change("HDMI-0", "2560x1440", "59.95", tool.parse_outputs(SAMPLE))
            self.assertEqual(apply.call_args_list[-1].args, ("HDMI-0", "1920x1080", "60.00"))

    def test_watchdog_restores_only_unconfirmed_changes(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(tool, "apply") as apply:
            token = Path(folder) / "pending"
            token.touch()
            tool.restore(str(token), "HDMI-0", "1920x1080", "60.00")
            tool.restore(str(token), "HDMI-0", "1920x1080", "60.00")
            apply.assert_called_once()
            self.assertFalse(token.exists())

    def test_apply_failure_restores_and_cancels_watchdog(self):
        with patch.object(tool.subprocess, "Popen") as watchdog, patch.object(tool, "apply", side_effect=[subprocess.CalledProcessError(1, "xrandr"), None]) as apply:
            with self.assertRaises(subprocess.CalledProcessError):
                tool.change("HDMI-0", "2560x1440", "59.95", tool.parse_outputs(SAMPLE))
            self.assertEqual(apply.call_count, 2)
            self.assertFalse(Path(watchdog.call_args.args[0][3]).exists())


if __name__ == "__main__":
    unittest.main()
