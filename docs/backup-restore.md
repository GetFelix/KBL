# Backup and restore

How to back up KBL's work log and restore it into a fresh Felix, and what
the backup does not hold. The log is the source of truth: every item, claim,
decision, price and budget is a fold of `work.events`, so a restored log
rebuilds the same state.

## Back up

With the tracker stopped, or at least quiet, so no record lands during the read:

```bash
kbl-tool work export work.backup
kbl-tool work digest --state before.json   # prints {"records":..,"at":..,"sha256":..}
```

The file's first line is a header with the record count, the first offset and a
SHA-256 over the records. The rest is the log, one record per line, as written.
Keep the digest with the file: it is what you compare a restore against.
`--state` also writes the fold as canonical JSON, to see what differs if two
digests do. The tool talks to Felix with the same `KBL_*` settings as the rest
of `kbl-tool` (`--tenant`, `--namespace`, `--broker`, `--broker-ca`); a team's
log is the namespace named by `--namespace`.

## Restore

Into a fresh Felix with the tenant and namespace set up (`dev/up.sh` does this
for the development stack):

1. Start the tracker once, then stop it. This creates `work.events` and the
   ready lanes. It must not run while you import: it would not see records
   that appear under it.
2. Import into the empty log:

   ```bash
   kbl-tool work import work.backup
   ```

   The import refuses a file that was cut short or edited (the header's count
   and checksum must match), refuses a log that already holds records, and
   stops if the broker gives a record any offset other than the one it had.
3. Check the fold:

   ```bash
   kbl-tool work digest --state after.json   # same "sha256" as before
   ```

4. Start the tracker. It rebuilds from the log and takes the writer lease,
   and items that were waiting are handed out.

The restore is held to the digest, not to the state's `PartialEq`. The digest is
a SHA-256 of the fold serialized with sorted keys, private fields included.
`dev/check-backup.mjs` runs this in CI: it fills a stack with claims, posts,
notes, decisions, steps, usage, prices, questions and answers, findings and
a reviewed pull request, backs it up, recreates the stack, restores, and compares
the log byte for byte and the two folds' digests and canonical JSON. It then
starts a tracker on the restored log and has an agent claim the item that was
waiting.

## What a backup covers

| Covered | Not covered |
| --- | --- |
| `work.events` of one namespace: everything the fold reads | Each run's log and tasks stream, and the artifacts cache that holds long step output: `kbl-tool run log <run>` prints a run's log, but there is no import for it |
| | The organization's log and a team's tokens: a team's `work.events` is covered by naming its namespace, but the org log is not |
| | Felix's own state: the tenant, grants and resources come from `dev/up.sh` or your control plane, and tracker tokens from the tracker's token file |
| | The ready lanes and the writer lease: rebuilt by the tracker |

A restore into a fresh Felix gives every record the offset it had, because the
broker numbers a new stream in order and the import checks each one.
A log with a gap cannot be exported. Restoring into a log that already has
records is not supported: restore into a fresh one.
