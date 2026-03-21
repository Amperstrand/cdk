#!/usr/bin/env python3

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from dataclasses import dataclass
from typing import Any
from urllib.error import HTTPError
from urllib.request import Request, urlopen


@dataclass
class Config:
    base_url: str
    rpc_addr: str
    rpc_token: str
    rpc_cli_bin: str
    alice_sats: int
    bob_sats: int
    timeout_secs: int


def env_required(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"missing required environment variable: {name}")
    return value


def load_config() -> Config:
    return Config(
        base_url=env_required("VOTING_E2E_BASE_URL").rstrip("/"),
        rpc_addr=env_required("VOTING_E2E_RPC_ADDR"),
        rpc_token=env_required("VOTING_E2E_RPC_TOKEN"),
        rpc_cli_bin=os.environ.get("VOTING_E2E_RPC_CLI_BIN", "target/debug/cdk-mint-cli"),
        alice_sats=int(os.environ.get("VOTING_E2E_ALICE_SATS", "17")),
        bob_sats=int(os.environ.get("VOTING_E2E_BOB_SATS", "23")),
        timeout_secs=int(os.environ.get("VOTING_E2E_TIMEOUT_SECS", "45")),
    )


def request_json(method: str, url: str, payload: dict[str, Any] | None = None) -> dict[str, Any]:
    data = None
    headers = {}
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
        headers["content-type"] = "application/json"

    req = Request(url=url, data=data, method=method, headers=headers)
    with urlopen(req, timeout=20) as response:
        return json.loads(response.read().decode("utf-8"))


def create_mint_quote(base_url: str, sats: int, description: str) -> dict[str, Any]:
    return request_json(
        "POST",
        f"{base_url}/v1/mint/quote/bolt11",
        {"unit": "sat", "amount": sats, "description": description},
    )


def get_mint_quote(base_url: str, quote_id: str) -> dict[str, Any]:
    return request_json("GET", f"{base_url}/v1/mint/quote/bolt11/{quote_id}")


def approve_quote(rpc_cli_bin: str, rpc_addr: str, rpc_token: str, quote_id: str) -> None:
    env = os.environ.copy()
    env["CDK_MINT_RPC_TOKEN"] = rpc_token
    subprocess.run(
        [
            rpc_cli_bin,
            "--addr",
            rpc_addr,
            "update-nut04-quote-state",
            quote_id,
            "PAID",
        ],
        check=True,
        env=env,
    )


def wait_state(base_url: str, quote_id: str, expected: str, timeout_secs: int) -> None:
    start = time.time()
    while True:
        state = get_mint_quote(base_url, quote_id).get("state")
        if state == expected:
            return
        if time.time() - start > timeout_secs:
            raise RuntimeError(
                f"quote {quote_id} did not reach state {expected}; last state={state}"
            )
        time.sleep(1)


def assert_lnurl_endpoints(base_url: str) -> None:
    for option in ("red", "blue"):
        payload = request_json("GET", f"{base_url}/.well-known/lnurlp/{option}")
        if payload.get("tag") != "payRequest":
            raise RuntimeError(f"invalid lnurl metadata for {option}: {payload}")
        callback = payload.get("callback", "")
        if not callback.endswith(f"/lnurl/cb/{option}"):
            raise RuntimeError(f"invalid callback for {option}: {callback}")

    callback_payload = request_json("GET", f"{base_url}/lnurl/cb/red?amount=10000")
    pr = callback_payload.get("pr", "")
    if not isinstance(pr, str) or not pr.lower().startswith("ln"):
        raise RuntimeError(f"invalid callback invoice payload: {callback_payload}")


def assert_vote_quote(base_url: str) -> None:
    quote = request_json(
        "POST",
        f"{base_url}/v1/melt/quote/vote",
        {
            "method": "vote",
            "request": "RED",
            "unit": "sat",
            "melt_options": {"amountless": {"amount_msat": 10000}},
        },
    )
    if quote.get("amount") != 10:
        raise RuntimeError(f"unexpected vote quote amount: {quote}")
    fee = quote.get("fee_reserve")
    if not isinstance(fee, int) or fee < 1:
        raise RuntimeError(f"unexpected vote quote fee: {quote}")


def assert_bolt11_vote_fee_behavior(base_url: str) -> None:
    vote_invoice_quote = create_mint_quote(base_url, 11, "red@inr2.cashu.exchange")
    normal_invoice_quote = create_mint_quote(base_url, 11, "not-a-vote")

    vote_melt_quote = request_json(
        "POST",
        f"{base_url}/v1/melt/quote/bolt11",
        {"request": vote_invoice_quote["request"], "unit": "sat"},
    )
    normal_melt_quote = request_json(
        "POST",
        f"{base_url}/v1/melt/quote/bolt11",
        {"request": normal_invoice_quote["request"], "unit": "sat"},
    )

    if int(vote_melt_quote.get("fee_reserve", -1)) < int(normal_melt_quote.get("fee_reserve", -1)):
        raise RuntimeError(
            "vote-like bolt11 description fee must be >= normal fee; "
            f"vote={vote_melt_quote}, normal={normal_melt_quote}"
        )


def main() -> int:
    try:
        config = load_config()

        alice = create_mint_quote(config.base_url, config.alice_sats, "CI Alice voting tokens")
        bob = create_mint_quote(config.base_url, config.bob_sats, "CI Bob voting tokens")

        alice_id = str(alice["quote"])
        bob_id = str(bob["quote"])

        if get_mint_quote(config.base_url, alice_id).get("state") != "UNPAID":
            raise RuntimeError(f"Alice quote not UNPAID before approval: {alice}")
        if get_mint_quote(config.base_url, bob_id).get("state") != "UNPAID":
            raise RuntimeError(f"Bob quote not UNPAID before approval: {bob}")

        approve_quote(config.rpc_cli_bin, config.rpc_addr, config.rpc_token, alice_id)
        approve_quote(config.rpc_cli_bin, config.rpc_addr, config.rpc_token, bob_id)

        wait_state(config.base_url, alice_id, "PAID", config.timeout_secs)
        wait_state(config.base_url, bob_id, "PAID", config.timeout_secs)

        assert_lnurl_endpoints(config.base_url)
        assert_vote_quote(config.base_url)
        assert_bolt11_vote_fee_behavior(config.base_url)

        print("live voting e2e passed")
        print(f"alice_quote={alice_id}")
        print(f"bob_quote={bob_id}")
        return 0
    except HTTPError as error:
        print(f"http error: status={error.code} url={error.url}", file=sys.stderr)
        return 1
    except Exception as error:  # noqa: BLE001
        print(f"e2e failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
