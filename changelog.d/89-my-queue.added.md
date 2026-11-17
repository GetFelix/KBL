Added `GET /v1/work/queue`, My queue as a read: every item waiting on a person, oldest wait first (waits begun before a snapshot first of all), then tier. `?limit=` defaults to 200, at most 1000.
