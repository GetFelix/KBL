Agents report spend as `usage` per model, in tokens (input, output, cache read,
cache write) or AI credits, instead of a `cost_micros` they worked out
themselves. The fold prices usage with the price in effect when it was written,
from `price_set` records a person writes with `POST /v1/work/prices`; budgets and
the floor are in the same derived cost. A model with no price is charged the floor
until a person prices it, then the rest is drawn. A report that still sends
`cost_micros` is refused with a message that says to send `usage`. This is log
format 8: until a person raises the log, reports are charged the floor and usage
is dropped. Spend gains `by_model` and `unpriced`.
