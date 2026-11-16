`kbl-tool prices import <file>` writes `price_set` records from a price table and
skips prices already in effect; `--update` writes one that differs, `--dry-run`
only says what it would do. `--default` imports the table shipped with the tool
(`tool/prices/default.json`: dated Anthropic list prices, to be checked against
the vendor's page), so a fresh install is not left with every model unpriced.
