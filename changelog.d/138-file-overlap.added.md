Overlapping files. An item can declare the files it expects to touch in a
`files` context entry. When two open items share a path, the dashboard puts a
"1 file overlap" flag on both rows, and the item panel's "Same files" section
names the other item (a link) and the shared files and suggests which should
wait on the other. The warning is derived from the fold, so it adds no record
and no format raise.
Only items in the same repo are compared, held work is suggested before work
in review and that before work not yet started, and only `./`, `/` or `.`
names the whole repo.
