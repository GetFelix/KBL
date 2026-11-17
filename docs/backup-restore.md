# Backup and restore

How to back up KBL's work log and restore it into a fresh Felix, and what
the backup does not hold. The log is the source of truth: every item, claim,
decision, price and budget is a fold of `work.events`, so a restored log
rebuilds the same state.

## Back up

With the tracker stopped, or at least quiet, so no record lands during the read:

```bash
kbl-tool work export work.backup          # prints the digest of what it wrote
kbl-tool org export org.backup            # team mode: the organization's log
KBL_NAMESPACE=alpha kbl-tool work export alpha.backup   # one per team
```

The file's first line is a header with the first and next offset, the record
count and a SHA-256 over the lines. Below it is one line per offset: the record
as written, or an empty line where the log has no record. A Raft shard leaves
such a blank at each refused conditional publish, so a log with blanks is
backed up with them in place. The file is created readable by its owner only.

Keep the digest with the file: it is what you compare a restore against. To
digest again later, `kbl-tool work digest --backup work.backup` reads only the
offsets the file covers, so a record written after the export does not make the
two disagree. `--state FILE` also writes the fold as canonical text, to see
what differs if two digests do. The tool talks to Felix with the same `KBL_*`
settings as the rest of `kbl-tool` (`--tenant`, `--namespace`, `--broker`,
`--broker-ca`). A team's log is the namespace named by `--namespace`; the org
log is always the organization's own namespace.

## Restore

Into a fresh Felix with the tenant and namespace set up (`dev/up.sh` does this
for the development stack). With one team and no organization, steps 1, 3, 5
and 6 are all there is:

1. Team mode only: `kbl-tool org init --tracker-subject <the tracker's subject>`.
2. Single team only: start the tracker once, then stop it. This creates
   `work.events` and the ready lanes. It must not run while you import: it
   would not see records that appear under it. In team mode skip this step;
   step 4 is the first start.
3. Team mode only: `kbl-tool org import org.backup`. It holds the teams, who is
   in them, and the hash of every agent and ingest token, so the tokens that
   were handed out work again. No token itself is ever in a backup.
4. Team mode only: start the tracker, run `kbl-tool team create <team>` for
   each team, and stop the tracker. Start it only after step 3: a tracker on an
   empty org log writes its first team there, and the import would then refuse
   the backup. The org log already lists the team, so this only creates its
   namespace and grants in the new Felix and lets the tracker open its streams.
5. Import each team's log into its empty `work.events`:

   ```bash
   kbl-tool work import work.backup
   KBL_NAMESPACE=alpha kbl-tool work import alpha.backup
   ```

   The import refuses a file that was cut short or edited (the header's count
   and checksum must match) and a log that holds a record the backup does not.
   It stops if the broker gives a record any offset other than the one it had,
   and checks the whole log against the file when it is done.
6. Check the fold, then start the tracker. It rebuilds from the log and takes the
   writer lease, and items that were waiting are handed out:

   ```bash
   kbl-tool work digest --state after.json   # same "sha256" as before
   kbl-tool org digest                       # team mode, same as the export's
   ```

If an import stops part way (a dropped connection, a refused blank), run it
again. A log that holds the start of the same backup carries on after it, and
one that already holds all of it says so and changes nothing.

Blanks are restored by a conditional publish that is refused on purpose. On a
Felix Raft shard that consumes exactly one offset, which is the blank. A broker
whose refusal consumes none (the hybrid engine) cannot reproduce a blank, so an
import of a backup with blanks stops there with a message saying so. The
records before the first blank are already in, and the import resumes on a Raft
broker.

The restore is held to the digest, not to the state's `PartialEq`. The work
digest is a SHA-256 of the fold serialized with sorted keys, private fields
included; the org digest is a SHA-256 of the org log's records with their
offsets, since that log is all the org fold reads.
`dev/check-backup.mjs` runs this in CI: it fills a stack in team mode with
claims, posts, notes, decisions, steps, usage, prices, questions and answers,
findings and a reviewed pull request, adds a second team with an agent token,
backs up all three logs, recreates the stack, restores, and compares each log
and the folds' digests. It then starts a tracker on the restored logs, has an
agent claim the item that was waiting, and has the restored agent token take
the second team's item.

## What a backup covers

| Covered | Not covered |
| --- | --- |
| `work.events` of each team you export, and `org.events`: everything the folds read | Each run's log and tasks stream, and the artifacts cache that holds long step output: `kbl-tool run log <run>` prints a run's log, but there is no import for it |
| Teams, memberships, roles and token hashes (the org log) | Tokens that are not in the org log: the static tokens file of a tracker without an organization is configuration you keep yourself |
| | Felix's own state: the tenant, grants and resources come from `dev/up.sh` or your control plane |
| | The ready lanes and the writer leases: rebuilt by the tracker |

A restore into a fresh Felix gives every record the offset it had, because the
import places each one and checks it. Restoring into a log that holds records
the backup does not is not supported: restore into a fresh one.
