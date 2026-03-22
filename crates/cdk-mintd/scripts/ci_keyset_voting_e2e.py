#!/usr/bin/env python3

from __future__ import annotations

import json
import os
import sys
from dataclasses import dataclass
from typing import Any
from urllib.error import HTTPError
from urllib.request import Request, urlopen


@dataclass
class Config:
    base_url: str
    expected_vote_units: list[str]


def env_required(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"missing required environment variable: {name}")
    return value


def load_config() -> Config:
    base_url = env_required("KEYSET_VOTING_E2E_BASE_URL").rstrip("/")
    units_raw = env_required("KEYSET_VOTING_E2E_EXPECTED_UNITS")

    expected_vote_units = [
        part.strip().lower() for part in units_raw.split(",") if part.strip()
    ]
    if not expected_vote_units:
        raise RuntimeError("KEYSET_VOTING_E2E_EXPECTED_UNITS cannot be empty")

    return Config(base_url=base_url, expected_vote_units=expected_vote_units)


def request_json(method: str, url: str, payload: dict[str, Any] | None = None) -> dict[str, Any]:
    data = None
    headers = {}
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
        headers["content-type"] = "application/json"

    req = Request(url=url, data=data, method=method, headers=headers)
    with urlopen(req, timeout=20) as response:
        return json.loads(response.read().decode("utf-8"))


def get_keysets(base_url: str) -> dict[str, Any]:
    return request_json("GET", f"{base_url}/v1/keysets")


def get_votes(base_url: str) -> dict[str, Any]:
    return request_json("GET", f"{base_url}/v1/votes")


def assert_vote_keysets_present(keysets_payload: dict[str, Any], expected_units: list[str]) -> list[dict[str, Any]]:
    keysets = keysets_payload.get("keysets")
    if not isinstance(keysets, list):
        raise RuntimeError(f"invalid /v1/keysets payload: {keysets_payload}")

    by_unit = {}
    for keyset in keysets:
        unit = str(keyset.get("unit", "")).lower()
        by_unit.setdefault(unit, []).append(keyset)

    missing = [unit for unit in expected_units if unit not in by_unit]
    if missing:
        raise RuntimeError(f"missing expected vote keyset units: {missing}")

    inactive = []
    selected = []
    for unit in expected_units:
        unit_keysets = by_unit[unit]
        active = [ks for ks in unit_keysets if bool(ks.get("active", False))]
        if not active:
            inactive.append(unit)
        else:
            selected.extend(active)

    if inactive:
        raise RuntimeError(f"expected vote units present but inactive: {inactive}")

    return selected


def assert_votes_endpoint_shape(votes_payload: dict[str, Any]) -> None:
    required = ["topic", "votes", "tally", "finalized"]
    missing = [field for field in required if field not in votes_payload]
    if missing:
        raise RuntimeError(
            f"/v1/votes missing required fields: {missing}; payload={votes_payload}"
        )

    if not isinstance(votes_payload.get("votes"), list):
        raise RuntimeError(f"/v1/votes field 'votes' must be list: {votes_payload}")

    if not isinstance(votes_payload.get("tally"), dict):
        raise RuntimeError(f"/v1/votes field 'tally' must be object: {votes_payload}")


def main() -> int:
    try:
        config = load_config()

        keysets_payload = get_keysets(config.base_url)
        active_vote_keysets = assert_vote_keysets_present(
            keysets_payload, config.expected_vote_units
        )

        votes_payload = get_votes(config.base_url)
        assert_votes_endpoint_shape(votes_payload)

        print("=" * 60)
        print("KEYSET VOTING E2E CHECK PASSED")
        print("=" * 60)
        print(f"base_url: {config.base_url}")
        print(f"expected_vote_units: {', '.join(config.expected_vote_units)}")
        print(f"active_vote_keysets: {len(active_vote_keysets)}")
        for keyset in active_vote_keysets:
            print(
                f"  - unit={keyset.get('unit')} id={keyset.get('id')} active={keyset.get('active')}"
            )
        print(f"votes_topic: {votes_payload.get('topic')}")
        print(f"votes_recorded: {len(votes_payload.get('votes', []))}")
        print(f"votes_finalized: {votes_payload.get('finalized')}")
        print("=" * 60)
        return 0
    except HTTPError as error:
        print(f"http error: status={error.code} url={error.url}", file=sys.stderr)
        return 1
    except Exception as error:  # noqa: BLE001
        print(f"keyset voting e2e failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
