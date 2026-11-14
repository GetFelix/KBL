Format 2 and its first record, `returned_to_scope`. When an issue that left its
intake rule matches it again, ingest writes it and the item leaves Needs you for
the stage it had; a person can write it too, to keep an item whose issue no
longer matches. The tracker refuses it until a person raises each team's log
to format 2 (`format_raised`, see docs/upgrades.md), and only after every
tracker and dashboard page reading that log is this build or newer. Until the
raise nothing changes: a returning item stays in Needs you.
