Run log records and tasks carry a format (`v`, left out at format 1, so
nothing written changes). A worker that claims a task of a newer format hands
it back with an error instead of running it without the part it cannot read,
and `record::read` tells a reader of a run log when a record is newer than its
build. docs/upgrades.md says why the cache values need no format.
