---
type: skill
id: stock_quote
description: Live market quote for a listed customer/peer, proxied read-only through an openapi backend against the Yahoo Finance chart API. The instance id is the ticker symbol.
backend:
  kind: openapi
  # Admin-registered endpoint NAME (base URL + auth live server-side, never
  # in markdown). scripts/demo-setup.sh registers `yahoo_finance` pointing
  # at https://query1.finance.yahoo.com — see sources/yahoo-finance-openapi.json
  # for the spec of the operation this read binding calls.
  endpoint: yahoo_finance
  read: { path: "/v8/finance/chart/{id}?interval=1d&range=1d" }
  project:
    symbol: $.chart.result.0.meta.symbol
    price: $.chart.result.0.meta.regularMarketPrice
    currency: $.chart.result.0.meta.currency
    previous_close: $.chart.result.0.meta.chartPreviousClose
    exchange: $.chart.result.0.meta.exchangeName
optional_frontmatter: [symbol, price, currency]
---

# stock_quote

An **openapi-backed** skill: an instance is a live window onto a remote
object — nothing is materialised in DuckDB. `expand` performs the bound
`GET /v8/finance/chart/{id}` **live** and returns the projected fields
(`symbol`, `price`, `currency`, `previous_close`, `exchange`) as the
`backend_projection`; the overlay page only carries identity, links, and
notes. No `write:` op is declared, so the skill is read-only end to end.

The demo materialises `[[stock_quote::sap]]` — SAP SE, the platform vendor
in the Hoffmann-Automotive story — via `create_remote_instance`.

**Offline behaviour is part of the demo:** with no internet the live read
fails **closed** to a `backend_projection.issue` (never a fabricated
price), and `validate_endpoints` reports the endpoint `unreachable`.
