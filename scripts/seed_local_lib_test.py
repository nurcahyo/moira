"""Unit tests for scripts/seed_local_lib.py — the pure logic pulled out of
scripts/seed-local.sh. No server, no database, no network: `python3
scripts/seed_local_lib_test.py` (or `make test-seed-local`).

These exist because scripts/seed-local.sh itself has none: the only prior
evidence for its behaviour was a manual run against a live server. This
does not replace that — it cannot exercise the HTTP/DB round trip — but it
pins the two pieces of logic that were wrong or fragile enough to be worth
a reviewer catching them (base_url normalisation convergence, and the
routing-policy staleness match) so a future edit cannot silently reintroduce
either bug.
"""

from __future__ import annotations

import unittest

from seed_local_lib import Truncated, find_by, find_routing_policy, normalize_base_url


class NormalizeBaseUrlTest(unittest.TestCase):
    def test_no_trailing_slash_is_unchanged(self):
        self.assertEqual(normalize_base_url("http://127.0.0.1:8000/v1"), "http://127.0.0.1:8000/v1")

    def test_single_trailing_slash_is_stripped(self):
        self.assertEqual(normalize_base_url("http://127.0.0.1:8000/v1/"), "http://127.0.0.1:8000/v1")

    def test_multiple_trailing_slashes_are_all_stripped(self):
        # Rust's `trim_end_matches('/')` removes every trailing match, not just one.
        self.assertEqual(normalize_base_url("http://127.0.0.1:8000/v1///"), "http://127.0.0.1:8000/v1")

    def test_surrounding_whitespace_is_trimmed(self):
        self.assertEqual(normalize_base_url("  http://127.0.0.1:8000/v1  "), "http://127.0.0.1:8000/v1")

    def test_matches_server_normalisation_so_reuse_converges(self):
        # This is the regression this function exists to prevent: comparing a
        # raw MOIRA_SEED_BASE_URL against what the server stored must land on
        # the same string, or the seed script reports `update` every run and
        # never reports `reuse`.
        raw = "http://127.0.0.1:8000/v1/"
        server_stored = raw.strip().rstrip("/")  # what validate_provider_base_url produces
        self.assertEqual(normalize_base_url(raw), server_stored)


class FindByTest(unittest.TestCase):
    def test_finds_matching_row(self):
        data = {"data": [{"id": "a", "display_name": "x"}, {"id": "b", "display_name": "y"}], "pagination": {"has_more": False}}
        self.assertEqual(find_by(data, "display_name", "y"), "b")

    def test_absent_and_not_truncated_returns_none(self):
        data = {"data": [{"id": "a", "display_name": "x"}], "pagination": {"has_more": False}}
        self.assertIsNone(find_by(data, "display_name", "z"))

    def test_absent_and_truncated_raises(self):
        data = {"data": [{"id": "a", "display_name": "x"}], "pagination": {"has_more": True}}
        with self.assertRaises(Truncated):
            find_by(data, "display_name", "z")

    def test_present_but_page_also_truncated_still_returns_the_hit(self):
        # A hit on this page is conclusive regardless of has_more — only an
        # empty result is ambiguous.
        data = {"data": [{"id": "a", "display_name": "x"}], "pagination": {"has_more": True}}
        self.assertEqual(find_by(data, "display_name", "x"), "a")


class FindRoutingPolicyTest(unittest.TestCase):
    def _row(self, **overrides):
        row = {
            "id": "policy-1",
            "route_id": "route-1",
            "provider_id": "provider-1",
            "provider_model_id": "model-1",
            "version": 3,
        }
        row.update(overrides)
        return row

    def test_matches_on_route_and_provider_together(self):
        data = {"data": [self._row()], "pagination": {"has_more": False}}
        row = find_routing_policy(data, "route-1", "provider-1")
        self.assertEqual(row["id"], "policy-1")
        self.assertEqual(row["provider_model_id"], "model-1")
        self.assertEqual(row["version"], 3)

    def test_right_route_wrong_provider_does_not_match(self):
        data = {"data": [self._row(provider_id="provider-other")], "pagination": {"has_more": False}}
        self.assertIsNone(find_routing_policy(data, "route-1", "provider-1"))

    def test_absent_and_truncated_raises(self):
        data = {"data": [self._row(route_id="route-other")], "pagination": {"has_more": True}}
        with self.assertRaises(Truncated):
            find_routing_policy(data, "route-1", "provider-1")

    def test_returned_row_carries_provider_model_id_for_the_staleness_check(self):
        # This is the field the step-5 fix in seed-local.sh reads to decide
        # reuse vs PATCH: an existing policy matched on (route_id,
        # provider_id) alone can still pin a stale provider_model_id after
        # the model or base_url changed underneath it.
        data = {"data": [self._row(provider_model_id="stale-model")], "pagination": {"has_more": False}}
        row = find_routing_policy(data, "route-1", "provider-1")
        self.assertEqual(row["provider_model_id"], "stale-model")


if __name__ == "__main__":
    unittest.main()
