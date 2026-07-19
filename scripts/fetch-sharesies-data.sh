#!/usr/bin/env bash
# One-off personal export of Sharesies data via Sharesies' own undocumented internal web-app
# APIs (not published/public APIs, no ToS guarantee, no stable contract — it's what
# app.sharesies.com's own frontend calls). This is for occasionally dumping your own data to
# JSON and feeding it into Sure by hand (e.g. via CSV import), not a standing/scheduled
# integration: session cookies expire and there's no login flow here, so you re-capture
# SHARESIES_COOKIE from a logged-in browser whenever you want fresh data.
#
# Four phases, run in sequence into one export directory:
#   1. Fetch "investing activity" history (app.sharesies.com), paginated via ?page=N&limit=N,
#      confirmed live. Records only carry an opaque fund_id UUID per instrument, not a
#      name/ticker.
#   2. Fetch wallet transactions (app.sharesies.com) — deposits/withdrawals/transfers between a
#      linked bank account and the Sharesies wallet, dividends, fees, etc. Useful for matching
#      against a bank export (e.g. ASB) and for valuing sales. This endpoint does NOT take a
#      page/cursor param — confirmed live by probing it directly: page, offset, cursor, before,
#      after, starting_after, page_token, next, page_number, since, until, from_date, to_date,
#      min_timestamp, max_timestamp and state were all rejected as unrecognised fields
#      ({"errors":{"<field>":["Rogue field"]}}). It only accepts `limit` (and `filter`, for
#      filtering by transaction reason — see SHARESIES_TRADING_STATUSES-style filtering, unused
#      here) and always returns the N=limit most recent transactions regardless of any other
#      param, so there is no known way to fetch "the next batch" — see SHARESIES_WALLET_LIMIT.
#   3. Resolve fund_ids (from phase 1) to instrument details (symbol, name, exchange, currency)
#      via Sharesies' separate, also-undocumented data.sharesies.nz API — its own origin, with
#      its own short-lived Bearer auth (observed ~15min TTL). The token doesn't need manual
#      capture: app.sharesies.com/api/identity/distill-token mints a fresh one from the same
#      session cookie phases 1-2 use (confirmed live: GET with the session Cookie + the usual
#      x-* headers returns {"token": "...", "type": "identity_token"}).
#   4. Download each resolved instrument's logo (fully public, no auth, multi-year cache-control
#      per data.sharesies.nz, so these are only ever downloaded once).
#
# How to get the required values (Firefox/Chrome DevTools):
#   1. Log in to app.sharesies.com, open DevTools -> Network.
#   2. Reload the investing activity page for the account you want. Click any request to
#      .../investing-activity, open its Headers tab. SHARESIES_ACCOUNT_ID is the UUID in the
#      request path. SHARESIES_COOKIE is the *entire* Cookie request header value (this cookie
#      is shared across app.sharesies.com, so the same value covers phases 1-3).
#   3. Reload the wallet page. Click any request to .../transactions-v2 — SHARESIES_WALLET_ID is
#      the UUID in that request's path. It is NOT necessarily the same UUID as
#      SHARESIES_ACCOUNT_ID (confirmed on a live account: they differ).
# Treat SHARESIES_COOKIE especially as a secret: don't commit it, don't paste it into chat/tickets
# (a leaked cookie is a live, working session for whoever has it). Put these in a local .env file
# (already gitignored) and load it with `set -a; source .env; set +a` (this correctly handles the
# quoting/special characters in a cookie value — `export $(... | xargs)` style one-liners do not:
# xargs' own quote/whitespace parsing mangles a raw Cookie header), or export them directly in
# your shell.
#
# Usage:
#   ./scripts/fetch-sharesies-data.sh                    # full run: fetch + resolve + logos
#   ./scripts/fetch-sharesies-data.sh path/to/export-dir  # resume phases 3-4 on an existing
#                                                         # export (its activity.json and
#                                                         # wallet-transactions.json are reused,
#                                                         # phases 1-2 are skipped) — handy when
#                                                         # the bearer token expires mid-run,
#                                                         # since re-running from scratch would
#                                                         # mean needlessly re-hitting phases 1-2
#
# Required env vars:
#   SHARESIES_COOKIE      full `Cookie:` header value from a logged-in browser request
#   SHARESIES_ACCOUNT_ID  the account/portfolio UUID from the investing-activity request path
#   SHARESIES_WALLET_ID   the wallet UUID from the transactions-v2 request path (see above —
#                         do not assume this equals SHARESIES_ACCOUNT_ID)
#   (all three are only optional when resuming with SHARESIES_DATA_BEARER also set — see below)
#
# Optional env vars:
#   SHARESIES_STATE               terminal (default, settled trades) or pending — phase 1
#   SHARESIES_LIMIT                page size for phase 1 (default: 100)
#   SHARESIES_WALLET_LIMIT         how many wallet transactions to request in phase 2's single
#                                  request (default: 5000 — comfortably above a typical personal
#                                  account's total; confirmed working up to at least 10000 in
#                                  testing). If phase 2 warns that has_more is still true at this
#                                  limit, raise it and re-run; there's no other known way to get
#                                  the remainder (see phase 2 above).
#   SHARESIES_DATA_BEARER          skip minting and use this Bearer token as-is for phase 3
#   SHARESIES_TRADING_STATUSES    comma-separated (default: active,halt,closeonly,notrade,inactive,unknown)
#   SHARESIES_BATCH_SIZE           instrument IDs per phase-3 request (default: 200)
#   SHARESIES_LOGO_SIZE            wide|thumb|micro (default: wide — highest res of the three)
#   SHARESIES_API_VERSION          x-api-version header (default: 33)
#   SHARESIES_GIT_HASH             x-git-hash header (default: a known-good value seen in a real request)
#   SHARESIES_DEVICE_KEY           x-known-device-key header (default: a known-good value)
#   SHARESIES_VERSION              x-version header (default: 42506)
#   SHARESIES_USER_AGENT           User-Agent header (default: a recent Firefox UA string)
#   OUT_DIR                        where to write output (default: ./sharesies-export — a fixed,
#                                  reusable directory, gitignored; re-running overwrites/updates
#                                  it in place rather than creating a new timestamped one)
#
# If requests start failing (non-200), the cookie (or, for phase 3 only, the bearer token) has
# almost certainly expired — re-capture it from a live browser session and re-run. A 404 on
# phase 2 specifically likely means SHARESIES_WALLET_ID is wrong (see above).
set -euo pipefail
cd "$(dirname "$0")/.."
shopt -s nullglob

fail() { echo "error: $*" >&2; exit 1; }

# Defends against a stray wrapping `"..."` or '...' surviving whatever method was used to load
# these from a .env file (e.g. `grep | cut` keeps quotes verbatim; `source` strips them) — a
# quoted account ID or state would otherwise get spliced straight into the URL and 404.
strip_quotes() {
  local v=$1
  v="${v%\"}"; v="${v#\"}"
  v="${v%\'}"; v="${v#\'}"
  printf '%s' "$v"
}

# Decodes a JWT's payload segment and prints its `exp` claim (unix seconds), or nothing if the
# token isn't a well-formed JWT / has no exp — used to fail loudly on a stale token instead of
# surfacing as an opaque downstream 500 (e.g. a leftover SHARESIES_DATA_BEARER export from a
# previous run).
jwt_exp() {
  printf '%s' "$1" | cut -d. -f2 | jq -R '
    gsub("-";"+") | gsub("_";"/")
    | . + ("=" * ((4 - (length % 4)) % 4))
    | @base64d | fromjson | .exp // empty
  ' 2>/dev/null
}

ext_for_content_type() {
  case "$1" in
    image/png) echo png ;;
    image/jpeg) echo jpg ;;
    image/svg+xml) echo svg ;;
    image/webp) echo webp ;;
    image/gif) echo gif ;;
    *) echo bin ;;
  esac
}

command -v jq >/dev/null || fail "jq is required but not installed"
command -v curl >/dev/null || fail "curl is required but not installed"

RESUME_DIR="${1:-}"
if [[ -n "$RESUME_DIR" ]]; then
  RESUME=1
  OUT_DIR="$RESUME_DIR"
  [[ -f "$OUT_DIR/activity.json" ]] || fail "no activity.json in $OUT_DIR — pass an export directory produced by a previous run of this script"
  [[ -f "$OUT_DIR/wallet-transactions.json" ]] || fail "no wallet-transactions.json in $OUT_DIR — pass an export directory produced by a previous run of this script"
else
  RESUME=0
  OUT_DIR="${OUT_DIR:-./sharesies-export}"
fi

# Cookie/account/wallet are needed for phases 1-2 always, and account+cookie again for phase 3's
# token minting unless a bearer override is supplied — so they're only skippable when resuming
# AND overriding the bearer (phases 1-2 don't run at all when resuming).
NEED_COOKIE=1
[[ $RESUME -eq 1 && -n "${SHARESIES_DATA_BEARER:-}" ]] && NEED_COOKIE=0
if [[ $NEED_COOKIE -eq 1 ]]; then
  [[ -n "${SHARESIES_COOKIE:-}" ]] || fail "SHARESIES_COOKIE is not set (see header comment for how to get it)"
  [[ -n "${SHARESIES_ACCOUNT_ID:-}" ]] || fail "SHARESIES_ACCOUNT_ID is not set (see header comment for how to get it)"
fi
if [[ $RESUME -eq 0 ]]; then
  [[ -n "${SHARESIES_WALLET_ID:-}" ]] || fail "SHARESIES_WALLET_ID is not set (see header comment for how to get it — it is NOT necessarily the same UUID as SHARESIES_ACCOUNT_ID)"
fi

SHARESIES_ACCOUNT_ID=$(strip_quotes "${SHARESIES_ACCOUNT_ID:-}")
SHARESIES_COOKIE=$(strip_quotes "${SHARESIES_COOKIE:-}")
SHARESIES_WALLET_ID=$(strip_quotes "${SHARESIES_WALLET_ID:-}")
STATE=$(strip_quotes "${SHARESIES_STATE:-terminal}")
LIMIT=$(strip_quotes "${SHARESIES_LIMIT:-100}")
WALLET_LIMIT=$(strip_quotes "${SHARESIES_WALLET_LIMIT:-5000}")
API_VERSION=$(strip_quotes "${SHARESIES_API_VERSION:-33}")
GIT_HASH=$(strip_quotes "${SHARESIES_GIT_HASH:-81e16b2c13f9a04701c175059ed444e4f94c3a62}")
DEVICE_KEY=$(strip_quotes "${SHARESIES_DEVICE_KEY:-00000000-0000-0000-0000-000000000000}")
APP_VERSION=$(strip_quotes "${SHARESIES_VERSION:-42506}")
USER_AGENT=$(strip_quotes "${SHARESIES_USER_AGENT:-Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:152.0) Gecko/20100101 Firefox/152.0}")
STATUSES="${SHARESIES_TRADING_STATUSES:-active,halt,closeonly,notrade,inactive,unknown}"
BATCH_SIZE="${SHARESIES_BATCH_SIZE:-200}"
LOGO_SIZE=$(strip_quotes "${SHARESIES_LOGO_SIZE:-wide}")
MAX_PAGES=500

mkdir -p "$OUT_DIR"

if [[ $RESUME -eq 1 ]]; then
  echo "Resuming in $OUT_DIR (phases 1-2 skipped, using existing activity.json / wallet-transactions.json)"
else
  echo "== Phase 1/4: fetching '$STATE' investing activity for account $SHARESIES_ACCOUNT_ID -> $OUT_DIR =="
  mkdir -p "$OUT_DIR/pages"

  page=1
  total_items=0
  while (( page <= MAX_PAGES )); do
    page_file="$OUT_DIR/pages/page-$(printf '%04d' "$page").json"
    url="https://app.sharesies.com/api/accounting/${SHARESIES_ACCOUNT_ID}/investing-activity?state=${STATE}&page=${page}&limit=${LIMIT}"

    http_code=$(curl -sS --compressed -o "$page_file" -w '%{http_code}' "$url" \
      -H "User-Agent: $USER_AGENT" \
      -H 'Accept: */*' \
      -H 'Accept-Language: en-NZ' \
      -H "Referer: https://app.sharesies.com/profile/personal/invest/${SHARESIES_ACCOUNT_ID}" \
      -H "x-api-version: $API_VERSION" \
      -H "x-git-hash: $GIT_HASH" \
      -H "x-known-device-key: $DEVICE_KEY" \
      -H "x-version: $APP_VERSION" \
      -H "Cookie: $SHARESIES_COOKIE")

    if [[ "$http_code" != "200" ]]; then
      fail "page $page returned HTTP $http_code (response saved to $page_file) — the session cookie has likely expired; re-capture it from a live browser request"
    fi

    # Real shape (confirmed against a live response): {has_more, page, state, type, records: [...]}.
    count=$(jq '.records | length' "$page_file") || fail "page $page did not return valid JSON — see $page_file"
    has_more=$(jq -r '.has_more' "$page_file")

    echo "page $page: $count item(s) (has_more=$has_more)"
    total_items=$(( total_items + count ))

    if (( count == 0 )); then
      rm -f "$page_file" # empty tail page, nothing worth keeping
      break
    fi

    if [[ "$has_more" != "true" ]]; then
      page=$(( page + 1 ))
      break
    fi

    page=$(( page + 1 ))
    sleep 0.5 # be a polite client of someone else's undocumented, unrate-limited-by-contract API
  done

  if (( page > MAX_PAGES )); then
    echo "warning: hit the $MAX_PAGES page safety cap — there may be more data than was fetched" >&2
  fi

  page_files=("$OUT_DIR"/pages/page-*.json)
  [[ ${#page_files[@]} -gt 0 ]] || fail "no investing-activity pages were fetched"
  jq -s '[.[] | .records[]]' "${page_files[@]}" > "$OUT_DIR/activity.json"
  rm -rf "$OUT_DIR/pages" # only useful for debugging a failed combine above; drop once combined

  echo "Phase 1 done. $total_items item(s) across $(( page - 1 )) page(s) -> $OUT_DIR/activity.json"

  echo "== Phase 2/4: fetching wallet transactions for wallet $SHARESIES_WALLET_ID -> $OUT_DIR =="
  wallet_raw="$OUT_DIR/wallet-transactions-raw.json"

  http_code=$(curl -sS --compressed -o "$wallet_raw" -w '%{http_code}' \
    "https://app.sharesies.com/api/wallet/${SHARESIES_WALLET_ID}/transactions-v2?limit=${WALLET_LIMIT}" \
    -H "User-Agent: $USER_AGENT" \
    -H 'Accept: */*' \
    -H 'Accept-Language: en-NZ' \
    -H "Referer: https://app.sharesies.com/profile/personal/wallet/${SHARESIES_WALLET_ID}" \
    -H "x-api-version: $API_VERSION" \
    -H "x-git-hash: $GIT_HASH" \
    -H "x-known-device-key: $DEVICE_KEY" \
    -H "x-version: $APP_VERSION" \
    -H "Cookie: $SHARESIES_COOKIE")

  if [[ "$http_code" != "200" ]]; then
    fail "wallet-transactions returned HTTP $http_code (response saved to $wallet_raw) — the session cookie has likely expired, or SHARESIES_WALLET_ID is wrong (a 404 usually means the latter); re-capture from a live browser request"
  fi

  jq -e . "$wallet_raw" >/dev/null 2>&1 || fail "wallet-transactions did not return valid JSON — see $wallet_raw"
  jq -e 'has("transactions")' "$wallet_raw" >/dev/null 2>&1 || fail "wallet-transactions response has no 'transactions' field — see $wallet_raw (the API shape may have changed since this script was written)"

  wallet_count=$(jq '.transactions | length' "$wallet_raw")
  wallet_has_more=$(jq -r '.has_more' "$wallet_raw")
  jq '.transactions' "$wallet_raw" > "$OUT_DIR/wallet-transactions.json"
  rm -f "$wallet_raw" # only useful for debugging a failed extract above; drop once extracted

  echo "Phase 2 done. $wallet_count item(s) -> $OUT_DIR/wallet-transactions.json"
  if [[ "$wallet_has_more" == "true" ]]; then
    echo "warning: has_more=true even at limit=$WALLET_LIMIT — this endpoint has no known page/cursor param (see header comment), it just returns the $WALLET_LIMIT most recent transactions; raise SHARESIES_WALLET_LIMIT and re-run to get the rest" >&2
  fi
fi

echo "== Phase 3/4: resolving instrument details =="

if [[ -n "${SHARESIES_DATA_BEARER:-}" ]]; then
  BEARER=$(strip_quotes "$SHARESIES_DATA_BEARER")
  echo "Using SHARESIES_DATA_BEARER from the environment (skipping auto-mint) — unset it if you want a fresh one minted automatically instead"
else
  token_file="$OUT_DIR/distill-token.json"
  http_code=$(curl -sS --compressed -o "$token_file" -w '%{http_code}' \
    'https://app.sharesies.com/api/identity/distill-token' \
    -H "User-Agent: $USER_AGENT" \
    -H 'Accept: */*' \
    -H 'Accept-Language: en-NZ' \
    -H "Referer: https://app.sharesies.com/profile/personal/invest/${SHARESIES_ACCOUNT_ID}" \
    -H "x-api-version: $API_VERSION" \
    -H "x-git-hash: $GIT_HASH" \
    -H "x-known-device-key: $DEVICE_KEY" \
    -H "x-version: $APP_VERSION" \
    -H "Cookie: $SHARESIES_COOKIE")

  if [[ "$http_code" != "200" ]]; then
    fail "distill-token returned HTTP $http_code (response saved to $token_file) — the session cookie has likely expired; re-capture it from a live browser request"
  fi

  BEARER=$(jq -r '.token // empty' "$token_file")
  [[ -n "$BEARER" ]] || fail "distill-token response had no .token field — see $token_file"
  rm -f "$token_file" # it's a live secret in its own right; don't leave it sitting on disk once read
  echo "Minted a fresh data.sharesies.nz token via distill-token"
fi

# Sanity-check the token we ended up with (minted or overridden) before spending a batch of
# requests on it — a wrong/expired token should fail loudly here, not as an opaque downstream 500.
[[ "$BEARER" =~ ^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$ ]] || fail "bearer token doesn't look like a JWT"
exp=$(jwt_exp "$BEARER")
if [[ -n "$exp" ]]; then
  now=$(date +%s)
  if (( exp <= now )); then
    fail "bearer token expired $(( now - exp ))s ago — if this came from SHARESIES_DATA_BEARER, unset it to auto-mint a fresh one instead"
  fi
fi

statuses_json=$(jq -Rn --arg s "$STATUSES" '[$s | split(",") | .[] | ltrimstr(" ") | rtrimstr(" ")]')

# Not readarray/mapfile: stock macOS ships bash 3.2, which predates both.
ids=()
while IFS= read -r id; do
  ids+=("$id")
done < <(jq -r '[.[].fund_id] | unique | .[]' "$OUT_DIR/activity.json")
[[ ${#ids[@]} -gt 0 ]] || fail "no fund_id values found in $OUT_DIR/activity.json"
echo "Resolving ${#ids[@]} unique instrument(s) -> $OUT_DIR"

mkdir -p "$OUT_DIR/instrument-batches"
batch_num=1
for ((i = 0; i < ${#ids[@]}; i += BATCH_SIZE)); do
  batch=("${ids[@]:i:BATCH_SIZE}")
  batch_file="$OUT_DIR/instrument-batches/batch-$(printf '%04d' "$batch_num").json"

  body=$(jq -n --argjson instruments "$(printf '%s\n' "${batch[@]}" | jq -R . | jq -s .)" \
               --argjson statuses "$statuses_json" \
    '{query: "", instruments: $instruments, tradingStatuses: $statuses, perPage: 500}')

  http_code=$(curl -sS --compressed -o "$batch_file" -w '%{http_code}' \
    'https://data.sharesies.nz/api/v1/instruments' \
    -X POST \
    -H "User-Agent: $USER_AGENT" \
    -H 'Accept: */*' \
    -H 'Accept-Language: en-NZ' \
    -H 'Content-Type: application/json' \
    -H 'Referer: https://app.sharesies.com/' \
    -H 'Origin: https://app.sharesies.com' \
    -H "authorization: Bearer $BEARER" \
    --data-raw "$body")

  if [[ "$http_code" != "200" ]]; then
    fail "batch $batch_num returned HTTP $http_code (response saved to $batch_file) — the bearer token likely expired mid-run (~15min TTL observed); re-run this script against $OUT_DIR to resume with a fresh one"
  fi

  returned=$(jq '.instruments | length' "$batch_file")
  echo "batch $batch_num: ${#batch[@]} id(s) requested, $returned returned -> $batch_file"
  batch_num=$(( batch_num + 1 ))
  sleep 0.5 # be a polite client of someone else's undocumented, unrate-limited-by-contract API
done

batch_files=("$OUT_DIR"/instrument-batches/batch-*.json)
jq -s '[.[] | .instruments[]] | unique_by(.id)' "${batch_files[@]}" > "$OUT_DIR/instruments.json"
jq '[.[] | {(.id): {symbol, name, exchange, currency}}] | add' "$OUT_DIR/instruments.json" > "$OUT_DIR/lookup.json"
rm -rf "$OUT_DIR/instrument-batches" # only useful for debugging a failed combine above; drop once combined

resolved=$(jq 'length' "$OUT_DIR/instruments.json")
echo "Phase 3 done. $resolved/${#ids[@]} instrument(s) resolved."
echo "Full records: $OUT_DIR/instruments.json"
echo "id -> {symbol,name,exchange,currency} lookup: $OUT_DIR/lookup.json"
if (( resolved < ${#ids[@]} )); then
  echo "warning: $(( ${#ids[@]} - resolved )) requested id(s) had no matching instrument" >&2
fi

echo "== Phase 4/4: downloading logos =="

logos_dir="$OUT_DIR/logos"
mkdir -p "$logos_dir"

logo_count=0
logo_skipped=0
while IFS=$'\t' read -r id symbol logo_path; do
  if [[ "$logo_path" == "null" || -z "$logo_path" ]]; then
    echo "warning: no '$LOGO_SIZE' logo for $symbol ($id)" >&2
    logo_skipped=$(( logo_skipped + 1 ))
    continue
  fi

  safe_symbol=$(printf '%s' "$symbol" | tr -c 'A-Za-z0-9._-' '_')
  existing=("$logos_dir/${safe_symbol}".*)
  if (( ${#existing[@]} > 0 )); then
    continue # logos are immutable per the cache-control above; don't re-fetch what we already have
  fi

  tmp_body=$(mktemp)
  tmp_headers=$(mktemp)
  http_code=$(curl -sS -D "$tmp_headers" -o "$tmp_body" -w '%{http_code}' \
    "https://data.sharesies.nz${logo_path}" \
    -H "User-Agent: $USER_AGENT" \
    -H 'Accept: */*')

  if [[ "$http_code" != "200" ]]; then
    echo "warning: logo download for $symbol ($id) returned HTTP $http_code" >&2
    rm -f "$tmp_body" "$tmp_headers"
    logo_skipped=$(( logo_skipped + 1 ))
    continue
  fi

  content_type=$(grep -i '^content-type:' "$tmp_headers" | tail -1 | cut -d: -f2- | tr -d ' \r\n')
  content_type=${content_type%%;*}
  ext=$(ext_for_content_type "$content_type")
  mv "$tmp_body" "$logos_dir/${safe_symbol}.${ext}"
  rm -f "$tmp_headers"
  logo_count=$(( logo_count + 1 ))
  sleep 0.1 # still someone else's server, even if this endpoint is public/cached
done < <(jq -r --arg size "$LOGO_SIZE" '.[] | [.id, .symbol, (.logos[$size] // "null")] | @tsv' "$OUT_DIR/instruments.json")

echo "Phase 4 done. $logo_count logo(s) downloaded, $logo_skipped skipped -> $logos_dir/"
echo
echo "All done. Export in $OUT_DIR/"
