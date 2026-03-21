# FakeWallet Voting Deployment (`inr2.cashu.exchange`)

This guide deploys `cdk-mintd` as a FakeWallet-backed mint with RED/BLUE voting over custom melt methods, behind HTTPS using Let's Encrypt.

## 1) Build `cdk-mintd` with fakewallet

From the repo root:

```bash
cargo build -p cdk-mintd --release --features fakewallet
```

Binary path:

```text
target/release/cdk-mintd
```

## 2) Server prep (`root@inr2.cashu.exchange`)

Install reverse proxy and TLS tooling:

```bash
apt update
apt install -y nginx certbot python3-certbot-nginx
```

Create runtime user and directories:

```bash
id -u cdk-mintd >/dev/null 2>&1 || useradd --system --home /var/lib/cdk-mintd --create-home --shell /usr/sbin/nologin cdk-mintd
mkdir -p /etc/cdk-mintd /var/lib/cdk-mintd
chown -R cdk-mintd:cdk-mintd /var/lib/cdk-mintd
```

## 3) Install binary

Copy binary from local machine:

```bash
scp target/release/cdk-mintd root@inr2.cashu.exchange:/usr/local/bin/cdk-mintd
ssh root@inr2.cashu.exchange 'chmod +x /usr/local/bin/cdk-mintd'
```

## 4) Configure `cdk-mintd`

Create `/etc/cdk-mintd/config.toml`:

```toml
[info]
url = "https://inr2.cashu.exchange/"
listen_host = "127.0.0.1"
listen_port = 8085
mnemonic = "replace with a secure 12-or-24-word mnemonic"

[info.quote_ttl]
mint_ttl = 600
melt_ttl = 120

[info.http_cache]
backend = "memory"
ttl = 60
tti = 60

[mint_info]
name = "INR2 Voting Mint"
description = "FakeWallet voting mint"
description_long = "Burn sats to vote RED or BLUE"

[database]
engine = "sqlite"

[ln]
ln_backend = "fakewallet"
min_mint = 1
max_mint = 500000
min_melt = 1
max_melt = 500000

[fake_wallet]
supported_units = ["sat"]
fee_percent = 0.0
reserve_fee_min = 0
min_delay_time = 0
max_delay_time = 1
manual_approval_incoming = false
voting_enabled = true
voting_options = ["RED", "BLUE"]
voting_topic = "Red vs Blue"
voting_fee_sat = 1

[limits]
max_inputs = 1000
max_outputs = 1000
```

Protect config:

```bash
chown root:cdk-mintd /etc/cdk-mintd/config.toml
chmod 640 /etc/cdk-mintd/config.toml
```

## 5) Systemd service

Create `/etc/systemd/system/cdk-mintd.service`:

```ini
[Unit]
Description=CDK Mint Daemon (FakeWallet Voting)
After=network.target

[Service]
Type=simple
User=cdk-mintd
Group=cdk-mintd
WorkingDirectory=/var/lib/cdk-mintd
ExecStart=/usr/local/bin/cdk-mintd --config /etc/cdk-mintd/config.toml --work-dir /var/lib/cdk-mintd
Restart=on-failure
RestartSec=5
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
systemctl daemon-reload
systemctl enable --now cdk-mintd
systemctl status cdk-mintd --no-pager
```

## 6) Nginx reverse proxy

Create `/etc/nginx/sites-available/inr2.cashu.exchange`:

```nginx
server {
    listen 80;
    server_name inr2.cashu.exchange;

    location / {
        proxy_pass http://127.0.0.1:8085;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

Enable site and reload:

```bash
ln -sf /etc/nginx/sites-available/inr2.cashu.exchange /etc/nginx/sites-enabled/inr2.cashu.exchange
nginx -t
systemctl reload nginx
```

## 7) Issue Let's Encrypt cert

```bash
certbot --nginx -d inr2.cashu.exchange --non-interactive --agree-tos -m admin@inr2.cashu.exchange --redirect
```

Verify renewal:

```bash
certbot renew --dry-run
```

## 8) Verify deployment

```bash
curl -sS https://inr2.cashu.exchange/v1/info | jq .name
curl -sS https://inr2.cashu.exchange/v1/info | jq '.nuts.nut05.methods'
```

You should see custom method `vote` available when using this FakeWallet voting setup.

## 9) Use the voting flow

Voting is implemented as custom melt method `vote`, with request string `RED` or `BLUE`.

Create a vote quote:

```bash
curl -sS -X POST https://inr2.cashu.exchange/v1/melt/quote/vote \
  -H 'content-type: application/json' \
  -d '{
    "method": "vote",
    "request": "RED",
    "unit": "sat",
    "melt_options": {
      "amountless": { "amount_msat": 10000 }
    }
  }' | jq .
```

Cast the vote by calling `/v1/melt/vote` with a valid quote and valid proofs (Cashu tokens). The vote weight equals melted sats.

## 10) Operations

Service logs:

```bash
journalctl -u cdk-mintd -f
```

Nginx logs:

```bash
journalctl -u nginx -f
```

Restart service:

```bash
systemctl restart cdk-mintd
```

## 11) Notes

- Vote tallies are in-memory in `cdk-fake-wallet` and do not persist across restarts.
- `voting_options` are case-insensitive in runtime logic.
- `voting_fee_sat` sets quote fee for vote melts.
