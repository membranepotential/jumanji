#!/usr/bin/env bash
# Publish a vault as the password-protected website (docs/publish/README.md,
# D15): build it with jumanji-publish into target/site, then deploy that
# folder and the password Worker (deploy/cloudflare/) with wrangler.
#
# The one-time setup (wrangler login, the SITE_PASSWORD secret) is in the doc.
# Wrangler is pinned: a version at least 7 days old (the dependency seasoning
# rule) and at least 4.44.0, so preview URLs follow the workers.dev setting.
#
# Usage: scripts/publish-site.sh VAULT_DIR [WORKER_NAME]
#   VAULT_DIR    the folder to publish; its index.md (else README.md) is home
#   WORKER_NAME  deploy under this Worker name instead of wrangler.jsonc's
#                (a second site; it needs its own SITE_PASSWORD secret)
set -euo pipefail

WRANGLER=wrangler@4.151.3

if [[ $# -lt 1 || $# -gt 2 ]]; then
  sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
  exit 2
fi
vault=$1
name=${2:-}

repo=$(cd "$(dirname "$0")/.." && pwd)
site="$repo/target/site"

cargo build --release --bin jumanji-publish --manifest-path "$repo/Cargo.toml"
"$repo/target/release/jumanji-publish" "$vault" -o "$site"

cd "$repo/deploy/cloudflare"
pnpm dlx "$WRANGLER" deploy ${name:+--name "$name"}
