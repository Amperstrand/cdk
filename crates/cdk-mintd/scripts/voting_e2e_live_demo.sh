#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${BASE_URL:-https://inr2.cashu.exchange}"
RPC_URL="${RPC_URL:-http://127.0.0.1:8086}"
RPC_SSH_HOST="${RPC_SSH_HOST:-root@inr2.cashu.exchange}"
ALICE_SATS="${ALICE_SATS:-50}"
BOB_SATS="${BOB_SATS:-70}"
AUTO_APPROVE="false"

if [[ "${1:-}" == "--auto-approve" ]]; then
  AUTO_APPROVE="true"
fi

require() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

require curl
require python3

json_field() {
  local json_input="$1"
  local field_path="$2"
  python3 - "$field_path" "$json_input" <<'PY'
import json
import sys

path = sys.argv[1].split('.')
obj = json.loads(sys.argv[2])
for key in path:
    obj = obj[key]
if isinstance(obj, bool):
    print("true" if obj else "false")
else:
    print(obj)
PY
}

echo "[1/5] Creating mint quotes for Alice and Bob"

alice_quote_json="$({
  curl -sS -X POST "${BASE_URL}/v1/mint/quote/bolt11" \
    -H 'content-type: application/json' \
    -d "{\"unit\":\"sat\",\"amount\":${ALICE_SATS},\"description\":\"Alice voting tokens\"}"
} )"
bob_quote_json="$({
  curl -sS -X POST "${BASE_URL}/v1/mint/quote/bolt11" \
    -H 'content-type: application/json' \
    -d "{\"unit\":\"sat\",\"amount\":${BOB_SATS},\"description\":\"Bob voting tokens\"}"
} )"

alice_quote_id="$(json_field "${alice_quote_json}" "quote")"
bob_quote_id="$(json_field "${bob_quote_json}" "quote")"
alice_invoice="$(json_field "${alice_quote_json}" "request")"
bob_invoice="$(json_field "${bob_quote_json}" "request")"

echo "Alice quote: ${alice_quote_id}"
echo "Bob quote:   ${bob_quote_id}"
echo
echo "Alice pays invoice in wallet: ${alice_invoice}"
echo "Bob pays invoice in wallet:   ${bob_invoice}"

echo
echo "[2/5] Operator approval (manual mint authorization)"
approve_cmd_alice="cdk-mint-cli --addr '${RPC_URL}' update-nut04-quote-state '${alice_quote_id}' PAID"
approve_cmd_bob="cdk-mint-cli --addr '${RPC_URL}' update-nut04-quote-state '${bob_quote_id}' PAID"

if [[ "${AUTO_APPROVE}" == "true" ]]; then
  echo "Auto-approving via SSH on ${RPC_SSH_HOST}"
  for quote_id in "${alice_quote_id}" "${bob_quote_id}"; do
    approved="false"
    for attempt in 1 2 3 4 5; do
      if ssh "${RPC_SSH_HOST}" "cdk-mint-cli --addr '${RPC_URL}' update-nut04-quote-state '${quote_id}' PAID"; then
        approved="true"
        break
      fi
      sleep 1
      echo "Retrying approval for ${quote_id} (attempt ${attempt}/5)"
    done

    if [[ "${approved}" != "true" ]]; then
      echo "Failed to approve quote ${quote_id} after retries" >&2
      exit 1
    fi
  done
else
  echo "Run these commands on operator host, then press Enter:"
  echo "  ${approve_cmd_alice}"
  echo "  ${approve_cmd_bob}"
  read -r
fi

echo
echo "[3/5] Verifying quote states are PAID"
alice_state="$(json_field "$(curl -sS "${BASE_URL}/v1/mint/quote/bolt11/${alice_quote_id}")" "state")"
bob_state="$(json_field "$(curl -sS "${BASE_URL}/v1/mint/quote/bolt11/${bob_quote_id}")" "state")"

echo "Alice state: ${alice_state}"
echo "Bob state:   ${bob_state}"

if [[ "${alice_state}" != "PAID" || "${bob_state}" != "PAID" ]]; then
  echo "Quotes are not fully approved yet. Aborting demo." >&2
  exit 1
fi

echo
echo "[4/5] Voting flows"
echo "Custom vote method examples (wallets supporting custom melts):"
echo "  RED  -> POST ${BASE_URL}/v1/melt/quote/vote with request=RED"
echo "  BLUE -> POST ${BASE_URL}/v1/melt/quote/vote with request=BLUE"

echo
echo "LN-address voting (wallet.cashu.me style):"
echo "  Alice sends to: red@inr2.cashu.exchange"
echo "  Bob sends to:   blue@inr2.cashu.exchange"

echo
echo "[5/5] Connectivity checks for ln-address endpoints"
python3 - <<'PY'
import json
import os
import urllib.request

base = os.environ["BASE_URL"]
for endpoint in ("red", "blue"):
    with urllib.request.urlopen(f"{base}/.well-known/lnurlp/{endpoint}", timeout=20) as response:
        data = json.loads(response.read().decode())
    trimmed = {
        "tag": data.get("tag"),
        "callback": data.get("callback"),
        "minSendable": data.get("minSendable"),
        "maxSendable": data.get("maxSendable"),
    }
    print(endpoint, json.dumps(trimmed, separators=(",", ":")))
PY

echo
echo "Demo setup complete."
echo "Next: each user mints approved tokens, then melts to RED/BLUE destination."
