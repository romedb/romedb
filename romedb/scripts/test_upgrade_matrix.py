#!/usr/bin/env python3
"""The upgrade table selects previous-release checks for a candidate."""
import json
from pathlib import Path
import tempfile
import unittest

import upgrade_matrix as matrix


class UpgradeMatrixTests(unittest.TestCase):
    def test_upgrade_groups_start_after_0_2_9(self):
        self.assertEqual(matrix.applicable("0.2.9"), [])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.10")], ["0.2.9"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.11")], ["0.2.9", "0.2.10"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.12")], ["0.2.9", "0.2.10", "0.2.11"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.13")], [])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.14")], ["0.2.13"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.15")], ["0.2.13", "0.2.14"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.16")], ["0.2.13", "0.2.14", "0.2.15"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.17")], ["0.2.13", "0.2.14", "0.2.15", "0.2.16"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.18")], ["0.2.13", "0.2.14", "0.2.15", "0.2.16", "0.2.17"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.19")], ["0.2.13", "0.2.14", "0.2.15", "0.2.16", "0.2.17", "0.2.18"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.20")], ["0.2.13", "0.2.14", "0.2.15", "0.2.16", "0.2.17", "0.2.18", "0.2.19"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.21")], ["0.2.13", "0.2.14", "0.2.15", "0.2.16", "0.2.17", "0.2.18", "0.2.19", "0.2.20"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.22")], ["0.2.13", "0.2.14", "0.2.15", "0.2.16", "0.2.17", "0.2.18", "0.2.19", "0.2.20", "0.2.21"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.23")], ["0.2.13", "0.2.14", "0.2.15", "0.2.16", "0.2.17", "0.2.18", "0.2.19", "0.2.20", "0.2.21", "0.2.22"])
        self.assertEqual([u.previous for u in matrix.applicable("0.2.24")], ["0.2.13", "0.2.14", "0.2.15", "0.2.16", "0.2.17", "0.2.18", "0.2.19", "0.2.20", "0.2.21", "0.2.22", "0.2.23"])
        self.assertEqual([u.previous for u in matrix.applicable("0.3.0")], ["0.2.13", "0.2.14", "0.2.15", "0.2.16", "0.2.17", "0.2.18", "0.2.19", "0.2.20", "0.2.21", "0.2.22", "0.2.23"])
        self.assertTrue(matrix.at_least("0.2.10", "0.2.9"))
        self.assertFalse(matrix.at_least("0.2.9", "0.2.10"))

    def test_rows_apply_from_their_first_version(self):
        row = matrix.Upgrade("v029", "0.2.9", "check-v251-upgrade.cjs", "0.2.10")
        original = matrix.UPGRADES
        matrix.UPGRADES = [row]
        try:
            self.assertEqual(matrix.applicable("0.2.9"), [])
            self.assertEqual(matrix.applicable("0.2.10"), [row])
            self.assertEqual(matrix.applicable("0.2.12"), [row])
            self.assertEqual(matrix.applicable("0.3.0"), [])
        finally:
            matrix.UPGRADES = original

    def test_a_format_break_stops_earlier_rows(self):
        self.assertEqual(matrix.applicable("0.2.13"), [])

    def test_missing_or_mismatched_previous_packages_fail(self):
        original = matrix.UPGRADES
        matrix.UPGRADES = [matrix.Upgrade("v029", "0.2.9", "check-v251-upgrade.cjs", "0.2.10")]
        try:
            with tempfile.TemporaryDirectory() as directory:
                package = Path(directory) / "pkg"
                package.mkdir()
                (package / "package.json").write_text(json.dumps({"version": "0.2.9"}))
                with self.assertRaisesRegex(AssertionError, "requires previous Node packages"):
                    matrix.previous_packages([], "0.2.10")
                with self.assertRaisesRegex(AssertionError, "not 0.2.8"):
                    matrix.previous_packages([f"0.2.8={package}"], "0.2.10")
                self.assertEqual(matrix.previous_packages([f"0.2.9={package}"], "0.2.10"), {"0.2.9": package.resolve()})
        finally:
            matrix.UPGRADES = original


if __name__ == "__main__":
    unittest.main()
