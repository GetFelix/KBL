The dashboard's Spend dialog lists each model and unit waiting for a price in a card, where a
signed-in person can set that unit (it is not a Needs-you item and blocks nothing), and
shows "check pricing" next to a model whose oldest price in effect is more than 90 days
old, counted from the import's `as_of` day (`?price_age_days=N` changes the age). A new
Models and prices page, from Spend or the palette, lists every model seen in recorded usage
with each unit's price, start, source, usage and history, and lets a person set or change a
price (a change in effect shows old to new and needs a confirm). The spend view gains
`prices` (every price set) and `unpriced` now names the unit. `price_set` carries an
optional `as_of`, which `kbl-tool prices import` fills from the table; a price with it is
format 9, one without stays format 8. The page asks again if the price changed under an open
form, and keeps focus and the saved message through a save.
