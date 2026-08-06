"""Pure logic shared by `scripts/seed-local.sh`, pulled out so it can be
unit-tested without a live server or database. See scripts/seed_local_lib_test.py.

Every function here is called from seed-local.sh via a `python3
scripts/seed_local_lib.py <subcommand> ...` invocation, reading JSON from
stdin where a list response is involved. Keep this file free of any I/O
beyond argv/stdin/stdout/stderr — that is what makes it testable without
Postgres or the API running.
"""

from __future__ import annotations

import json
import sys


class Truncated(Exception):
    """Raised when a list response was paginated and the row we need might
    be on a page we never fetched. The caller must treat this as "cannot
    tell whether it already exists", not as "does not exist"."""


def normalize_base_url(value: str) -> str:
    """Mirror `validate_provider_base_url` in src/application/admin/shared.rs:
    `value.trim().trim_end_matches('/')`. The server stores exactly this
    normalised form, so a caller comparing a base_url it holds against what
    the server returns must normalise the same way first — otherwise a URL
    that differs only by whitespace or a trailing slash never compares equal
    and the seed script reports `update` every run instead of `reuse`."""
    return value.strip().rstrip("/")


def find_routing_policy(data: dict, route_id: str, provider_id: str) -> dict | None:
    """Find the routing policy matching (route_id, provider_id) in a
    `{data: [...], pagination: {...}}` list response. Returns the full row
    (so the caller can read provider_model_id and version off it) or None
    if genuinely absent. Raises Truncated if the list was cut short and the
    row could be on a page never fetched — "not on this page" is not the
    same as "does not exist"."""
    hits = [
        r
        for r in data["data"]
        if r.get("route_id") == route_id and r.get("provider_id") == provider_id
    ]
    if hits:
        return hits[0]
    if (data.get("pagination") or {}).get("has_more"):
        raise Truncated(len(data["data"]))
    return None


def find_by(data: dict, field: str, want: str) -> str | None:
    """Find one row in a `{data: [...], pagination: {...}}` list response by
    a single field, returning its id. Same truncation rule as
    find_routing_policy."""
    hits = [r for r in data["data"] if r.get(field) == want]
    if hits:
        return hits[0]["id"]
    if (data.get("pagination") or {}).get("has_more"):
        raise Truncated(len(data["data"]))
    return None


def _cmd_normalize_base_url(argv: list[str]) -> int:
    print(normalize_base_url(argv[0]))
    return 0


def _cmd_find_by(argv: list[str]) -> int:
    field, want = argv[0], argv[1]
    data = json.load(sys.stdin)
    try:
        found = find_by(data, field, want)
    except Truncated as exc:
        sys.stderr.write(
            "   list truncated at %d rows; cannot tell whether %s=%s already exists.\n"
            "   Seeding again could duplicate it. Clean the database or seed by hand.\n"
            % (exc.args[0], field, want)
        )
        return 3
    print(found or "")
    return 0


def _cmd_find_routing_policy(argv: list[str]) -> int:
    route_id, provider_id = argv[0], argv[1]
    data = json.load(sys.stdin)
    try:
        row = find_routing_policy(data, route_id, provider_id)
    except Truncated as exc:
        sys.stderr.write(
            "   routing-policies truncated at %d rows; cannot tell whether a policy for\n"
            "   this route and provider already exists. Seeding again could duplicate it.\n"
            % exc.args[0]
        )
        return 3
    if row is None:
        print("")
    else:
        print("%s\t%s\t%s" % (row["id"], row["provider_model_id"], row["version"]))
    return 0


_COMMANDS = {
    "normalize-base-url": _cmd_normalize_base_url,
    "find-by": _cmd_find_by,
    "find-routing-policy": _cmd_find_routing_policy,
}


def main(argv: list[str]) -> int:
    if not argv or argv[0] not in _COMMANDS:
        sys.stderr.write(
            "usage: seed_local_lib.py {%s} ...\n" % "|".join(sorted(_COMMANDS))
        )
        return 2
    return _COMMANDS[argv[0]](argv[1:])


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
