//! The person tools, one entry each. Adding a tool is adding an entry: the
//! schema, the argument checks and the call to the tracker all come from it.
//! The tracker checks the token on every route named here as well.

/// What a parameter holds.
#[derive(Clone, Copy)]
pub(super) enum Kind {
    /// A string, sent as it is.
    Text,
    /// A whole number from 0.
    Number,
    /// `true` or `false`.
    Bool,
    /// One of these words.
    Words(&'static [&'static str]),
    /// A string the tracker keeps as content. Sent as `{"text": ...}`.
    Content,
    /// A person's name, sent as `{"person": ...}`, or `agent:<name>`, sent as `{"agent": ...}`.
    Assignee,
}

pub(super) struct Param {
    pub(super) name: &'static str,
    pub(super) kind: Kind,
    pub(super) required: bool,
    pub(super) about: &'static str,
}

/// How a tool reaches the tracker.
pub(super) enum Route {
    /// A write: the arguments are the body.
    Post(&'static str),
    /// A read: the arguments are the query.
    Get(&'static str),
}

pub(super) struct Tool {
    pub(super) name: &'static str,
    pub(super) about: &'static str,
    pub(super) route: Route,
    pub(super) params: &'static [Param],
}

const ITEM: Param = Param {
    name: "item",
    kind: Kind::Text,
    required: true,
    about: "The item's id.",
};

pub(super) const TOOLS: &[Tool] = &[
    Tool {
        name: "my_queue",
        about: "What waits on you in this team, oldest wait first, then lower tier. An item assigned to someone else is left out, and so is one you snoozed or parked while the wait you hid continues.",
        route: Route::Get("/v1/work/queue"),
        params: &[Param {
            name: "limit",
            kind: Kind::Number,
            required: false,
            about: "The most entries to return. Default 200, at most 1000.",
        }],
    },
    Tool {
        name: "accept_proposal",
        about: "Accept a proposal: it becomes work. Give `tier` to set its priority.",
        route: Route::Post("/v1/work/proposals/accept"),
        params: &[
            ITEM,
            Param {
                name: "tier",
                kind: Kind::Number,
                required: false,
                about: "0 (most urgent) to 3. Keeps the proposed tier when left out.",
            },
        ],
    },
    Tool {
        name: "reject_proposal",
        about: "Reject a proposal. It is not work and leaves the queue.",
        route: Route::Post("/v1/work/proposals/reject"),
        params: &[ITEM],
    },
    Tool {
        name: "answer_question",
        about: "Answer the question an item waits on, by choosing one of its options or in your own words. The item resumes for any agent to claim.",
        route: Route::Post("/v1/work/questions/answer"),
        params: &[
            ITEM,
            Param {
                name: "question",
                kind: Kind::Number,
                required: false,
                about: "The offset of the question. Defaults to the open one.",
            },
            Param {
                name: "chosen",
                kind: Kind::Text,
                required: false,
                about: "One of the question's options.",
            },
            Param {
                name: "text",
                kind: Kind::Content,
                required: false,
                about: "Your answer in words, when you choose no option.",
            },
        ],
    },
    Tool {
        name: "set_tier",
        about: "Change an item's priority.",
        route: Route::Post("/v1/work/tier"),
        params: &[
            ITEM,
            Param {
                name: "tier",
                kind: Kind::Number,
                required: true,
                about: "0 (most urgent) to 3.",
            },
        ],
    },
    Tool {
        name: "flag_item",
        about: "Flag an item so it stands out.",
        route: Route::Post("/v1/work/flag"),
        params: &[ITEM],
    },
    Tool {
        name: "unflag_item",
        about: "Take the flag off an item.",
        route: Route::Post("/v1/work/unflag"),
        params: &[ITEM],
    },
    Tool {
        name: "add_note",
        about: "Add a note, a review comment or a priority change to an item. The record names you.",
        route: Route::Post("/v1/work/note"),
        params: &[
            ITEM,
            Param {
                name: "text",
                kind: Kind::Content,
                required: true,
                about: "What you want the next agent to read.",
            },
            Param {
                name: "input",
                kind: Kind::Words(&["note", "review_comment", "priority_change"]),
                required: false,
                about: "What kind of input it is. Default `note`.",
            },
            Param {
                name: "attempt",
                kind: Kind::Number,
                required: false,
                about: "The attempt it steers. Left out, it is about the item as a whole.",
            },
        ],
    },
    Tool {
        name: "retry_item",
        about: "Give an item that used its tries a fresh set.",
        route: Route::Post("/v1/work/retry"),
        params: &[ITEM],
    },
    Tool {
        name: "redo_from_decision",
        about: "Start a new attempt from a decision an earlier attempt recorded. It counts against the root's redos per item, and the tracker refuses it for a closed item, one an agent holds, an unknown decision, or when no redos are left. After a failed check it needs a note written since.",
        route: Route::Post("/v1/work/redo"),
        params: &[
            ITEM,
            Param {
                name: "from_attempt",
                kind: Kind::Number,
                required: true,
                about: "The attempt that recorded the decision.",
            },
            Param {
                name: "at",
                kind: Kind::Number,
                required: true,
                about: "The offset of the decision.",
            },
            Param {
                name: "note",
                kind: Kind::Number,
                required: false,
                about: "The offset of a note (add_note) that steers the new attempt.",
            },
        ],
    },
    Tool {
        name: "snooze_item",
        about: "Hide an item from the queue until a time.",
        route: Route::Post("/v1/work/snooze"),
        params: &[
            ITEM,
            Param {
                name: "until_ms",
                kind: Kind::Number,
                required: true,
                about: "Milliseconds since the Unix epoch, in the future.",
            },
        ],
    },
    Tool {
        name: "block_item",
        about: "Hold an item back from every agent until a time, or on a named outside resource until you lift it. An agent already working it keeps its claim.",
        route: Route::Post("/v1/work/block"),
        params: &[
            ITEM,
            Param {
                name: "reason",
                kind: Kind::Text,
                required: true,
                about: "Why it is held, shown on the card.",
            },
            Param {
                name: "until_ms",
                kind: Kind::Number,
                required: false,
                about: "Milliseconds since the Unix epoch, in the future and within a year. The block ends then by itself.",
            },
            Param {
                name: "resource",
                kind: Kind::Text,
                required: false,
                about: "What it waits on, such as `azure-vms`: letters, digits, `-`, `_` and `.`. Give this or `until_ms`, or both.",
            },
        ],
    },
    Tool {
        name: "classify_item",
        about: "Set or override an item's type, area or size. The value must be in the team's vocabulary; an empty value clears it. Triage and intake rules never replace a value you set.",
        route: Route::Post("/v1/work/classify"),
        params: &[
            ITEM,
            Param {
                name: "item_type",
                kind: Kind::Text,
                required: false,
                about: "The type, such as `bug` or `feature`.",
            },
            Param {
                name: "area",
                kind: Kind::Text,
                required: false,
                about: "The code area.",
            },
            Param {
                name: "size",
                kind: Kind::Text,
                required: false,
                about: "The expected size, `S`, `M` or `L` unless the vocabulary lists others.",
            },
        ],
    },
    Tool {
        name: "lift_block",
        about: "End an item's block now, so agents are handed it again.",
        route: Route::Post("/v1/work/lift-block"),
        params: &[ITEM],
    },
    Tool {
        name: "park_item",
        about: "Hide an item from the queue until it waits for a new reason.",
        route: Route::Post("/v1/work/park"),
        params: &[ITEM],
    },
    Tool {
        name: "unpark_item",
        about: "Put a parked item back in the queue.",
        route: Route::Post("/v1/work/unpark"),
        params: &[ITEM],
    },
    Tool {
        name: "unblock_item",
        about: "Mark a dependency resolved, so the item no longer waits on it.",
        route: Route::Post("/v1/work/unblock"),
        params: &[
            ITEM,
            Param {
                name: "to",
                kind: Kind::Text,
                required: true,
                about: "The id of the item it waits on.",
            },
        ],
    },
    Tool {
        name: "relate_items",
        about: "Relate two items so they are scheduled together, for example because they should run on the same machines. A relation never holds either item back: both stay claimable. Use a dependency when one must be done first.",
        route: Route::Post("/v1/work/relate"),
        params: &[
            ITEM,
            Param {
                name: "to",
                kind: Kind::Text,
                required: true,
                about: "The id of the item to relate it to. The relation goes both ways.",
            },
        ],
    },
    Tool {
        name: "unrelate_items",
        about: "Remove a relation between two items.",
        route: Route::Post("/v1/work/unrelate"),
        params: &[
            ITEM,
            Param {
                name: "to",
                kind: Kind::Text,
                required: true,
                about: "The id of the item it is related to.",
            },
        ],
    },
    Tool {
        name: "request_review",
        about: "Record that the item's open pull request was put to a reviewer. The tracker contacts no one.",
        route: Route::Post("/v1/work/request-review"),
        params: &[
            ITEM,
            Param {
                name: "reviewer",
                kind: Kind::Text,
                required: true,
                about: "Who was asked, up to 100 characters.",
            },
        ],
    },
    Tool {
        name: "request_changes",
        about: "Record that a person sent the item's open pull request back.",
        route: Route::Post("/v1/work/request-changes"),
        params: &[ITEM],
    },
    Tool {
        name: "mark_merged",
        about: "Record a merge you saw upstream that ingest has not, and close the item as merged.",
        route: Route::Post("/v1/work/mark-merged"),
        params: &[ITEM],
    },
    Tool {
        name: "rate_summary",
        about: "Mark the summary an attempt handed off as accurate or not.",
        route: Route::Post("/v1/work/rate-summary"),
        params: &[
            ITEM,
            Param {
                name: "attempt",
                kind: Kind::Number,
                required: true,
                about: "The attempt whose summary this marks.",
            },
            Param {
                name: "accurate",
                kind: Kind::Bool,
                required: true,
                about: "Whether the summary was accurate.",
            },
        ],
    },
    Tool {
        name: "take_item",
        about: "Assign the item to yourself. No agent claims it while it is yours; an agent already holding it finishes or is stopped first.",
        route: Route::Post("/v1/work/take"),
        params: &[ITEM],
    },
    Tool {
        name: "reassign_item",
        about: "Assign the item to a person of this team, to one of its agents (only that agent may claim it), or to nobody so any agent may claim it.",
        route: Route::Post("/v1/work/reassign"),
        params: &[
            ITEM,
            Param {
                name: "to",
                kind: Kind::Assignee,
                required: false,
                about: "A person's name, or agent:<name> for an agent. Leave it out for nobody.",
            },
        ],
    },
    Tool {
        name: "steer_agent",
        about: "Send a note to the agent holding the item. It reads it as a note on the attempt it holds.",
        route: Route::Post("/v1/work/steer"),
        params: &[
            ITEM,
            Param {
                name: "attempt",
                kind: Kind::Number,
                required: true,
                about: "The attempt the agent holds.",
            },
            Param {
                name: "text",
                kind: Kind::Content,
                required: true,
                about: "The note.",
            },
        ],
    },
];
