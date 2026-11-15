Ingest opens the upstream issue for a prerequisite a person accepted, when the
target repo's intake rule sets `open_prerequisites: true`, and ties the item to
it so upstream sync follows it like any imported root. The tie is format 6: a
format 5 log and older builds are unchanged. A crash between opening the issue
and recording it is finished by the next poll, which finds the issue by its
`kbl-prerequisite:` marker and opens no second one. The waiting issue is named in
the new one only when the rule sets `link_waiter: true`.
