import unittest

from shipping_policy import check_security_review


class SecurityReviewTests(unittest.TestCase):
    def test_requires_review_for_exact_shipping_lockfile(self):
        receipt = {"cargo_lock_sha256": "a" * 64, "unresolved_count": 0}
        check_security_review(receipt, "a" * 64)
        with self.assertRaisesRegex(ValueError, "Cargo.lock"):
            check_security_review(receipt, "b" * 64)

    def test_requires_resolved_findings_status(self):
        receipt = {"cargo_lock_sha256": "a" * 64}
        for status in [None, 1]:
            receipt["unresolved_count"] = status
            with self.assertRaisesRegex(ValueError, "findings status"):
                check_security_review(receipt, "a" * 64)


if __name__ == "__main__":
    unittest.main()
