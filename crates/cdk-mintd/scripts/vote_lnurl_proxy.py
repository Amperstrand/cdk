#!/usr/bin/env python3
"""Minimal LNURL-pay proxy for RED/BLUE voting.

Exposes:
  - /.well-known/lnurlp/red
  - /.well-known/lnurlp/blue
  - /lnurl/cb/red?amount=<msat>
  - /lnurl/cb/blue?amount=<msat>

Callback creates a BOLT11 mint quote on cdk-mintd with invoice description
set to RED or BLUE, then returns that invoice as LNURL payRequest `pr`.
"""

from __future__ import annotations

import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.parse import parse_qs, urlparse
from urllib.request import Request, urlopen


HOST = os.environ.get("VOTE_LNURL_HOST", "127.0.0.1")
PORT = int(os.environ.get("VOTE_LNURL_PORT", "8090"))
DOMAIN = os.environ.get("VOTE_LNURL_DOMAIN", "inr2.cashu.exchange")
MINT_URL = os.environ.get("VOTE_LNURL_MINT_URL", "http://127.0.0.1:8085")
MIN_SENDABLE_MSAT = int(os.environ.get("VOTE_LNURL_MIN_SENDABLE_MSAT", "1000"))
MAX_SENDABLE_MSAT = int(os.environ.get("VOTE_LNURL_MAX_SENDABLE_MSAT", "100000000"))


def write_json(handler: BaseHTTPRequestHandler, status: int, body: dict) -> None:
    data = json.dumps(body).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(data)))
    handler.send_header("Access-Control-Allow-Origin", "*")
    handler.end_headers()
    handler.wfile.write(data)


def option_from_path(path: str) -> str | None:
    lowered = path.lower()
    if lowered.endswith("/red"):
        return "RED"
    if lowered.endswith("/blue"):
        return "BLUE"
    return None


def lnurl_metadata(option: str) -> str:
    identifier = f"{option.lower()}@{DOMAIN}"
    return json.dumps(
        [
            ["text/plain", f"Vote {option} on {DOMAIN}"],
            ["text/identifier", identifier],
        ],
        separators=(",", ":"),
    )


def create_mint_invoice(option: str, amount_msat: int) -> str:
    if amount_msat % 1000 != 0:
        raise ValueError("amount must be whole sats (msat multiple of 1000)")

    amount_sat = amount_msat // 1000
    payload = {
        "unit": "sat",
        "amount": amount_sat,
        "description": option,
    }
    req = Request(
        f"{MINT_URL}/v1/mint/quote/bolt11",
        data=json.dumps(payload).encode("utf-8"),
        headers={"content-type": "application/json"},
        method="POST",
    )
    with urlopen(req, timeout=20) as response:
        body = json.loads(response.read().decode("utf-8"))
    invoice = body.get("request")
    if not invoice:
        raise RuntimeError("mint did not return bolt11 request")
    return invoice


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802
        parsed = urlparse(self.path)
        option = option_from_path(parsed.path)

        if option is None:
            write_json(self, 404, {"status": "ERROR", "reason": "not found"})
            return

        if parsed.path.startswith("/.well-known/lnurlp/"):
            callback = f"https://{DOMAIN}/lnurl/cb/{option.lower()}"
            body = {
                "tag": "payRequest",
                "callback": callback,
                "minSendable": MIN_SENDABLE_MSAT,
                "maxSendable": MAX_SENDABLE_MSAT,
                "metadata": lnurl_metadata(option),
                "commentAllowed": 0,
                "allowsNostr": False,
            }
            write_json(self, 200, body)
            return

        if parsed.path.startswith("/lnurl/cb/"):
            params = parse_qs(parsed.query)
            amount_values = params.get("amount", [])
            if not amount_values:
                write_json(self, 400, {"status": "ERROR", "reason": "missing amount"})
                return

            try:
                amount_msat = int(amount_values[0])
            except ValueError:
                write_json(self, 400, {"status": "ERROR", "reason": "invalid amount"})
                return

            if amount_msat < MIN_SENDABLE_MSAT or amount_msat > MAX_SENDABLE_MSAT:
                write_json(self, 400, {"status": "ERROR", "reason": "amount out of range"})
                return

            try:
                pr = create_mint_invoice(option, amount_msat)
            except ValueError as error:
                write_json(self, 400, {"status": "ERROR", "reason": str(error)})
                return
            except HTTPError as error:
                write_json(
                    self,
                    502,
                    {
                        "status": "ERROR",
                        "reason": f"mint quote error: {error.code}",
                    },
                )
                return
            except Exception as error:  # noqa: BLE001
                write_json(
                    self,
                    502,
                    {"status": "ERROR", "reason": f"mint unavailable: {error}"},
                )
                return

            write_json(self, 200, {"pr": pr, "routes": []})
            return

        write_json(self, 404, {"status": "ERROR", "reason": "not found"})

    def log_message(self, fmt: str, *args) -> None:  # noqa: A003
        return


def main() -> None:
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    print(f"vote-lnurl-proxy listening on {HOST}:{PORT}")
    server.serve_forever()


if __name__ == "__main__":
    main()
