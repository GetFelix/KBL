Overlapping files. An item can declare the files it expects to touch in a
`files` context entry. When two open items share a path, the dashboard warns
on both and suggests which should wait on the other. The warning is derived
from the fold, so it adds no record and no format raise.
