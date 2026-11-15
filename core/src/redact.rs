//! Known secret patterns, cut out of what an agent reports before it is
//! written. Tool output and diffs can carry a token the agent read from the
//! environment or a file, and anything on `work.events` stays readable for
//! as long as the item is kept.
//!
//! This is a list of known shapes, not a guarantee: a secret with no
//! recognisable prefix goes through.

use std::borrow::Cow;

use crate::content::Content;
use crate::work::WorkRecord;

/// What a secret is replaced with.
pub const REDACTED: &str = "[redacted]";

/// A prefix, the shortest tail that makes it a secret, and the tail's bytes.
struct Shape {
    prefix: &'static str,
    min_tail: usize,
    tail: fn(u8) -> bool,
}

fn alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn token(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn upper(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}

fn slack(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

fn bearer(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b)
}

const SHAPES: &[Shape] = &[
    // GitHub tokens and fine-grained tokens.
    Shape {
        prefix: "ghp_",
        min_tail: 36,
        tail: alnum,
    },
    Shape {
        prefix: "gho_",
        min_tail: 36,
        tail: alnum,
    },
    Shape {
        prefix: "ghu_",
        min_tail: 36,
        tail: alnum,
    },
    Shape {
        prefix: "ghs_",
        min_tail: 36,
        tail: alnum,
    },
    Shape {
        prefix: "ghr_",
        min_tail: 36,
        tail: alnum,
    },
    Shape {
        prefix: "github_pat_",
        min_tail: 22,
        tail: word,
    },
    Shape {
        prefix: "glpat-",
        min_tail: 20,
        tail: token,
    },
    // Model provider keys: `sk-`, `sk-ant-`, `sk-proj-`.
    Shape {
        prefix: "sk-",
        min_tail: 20,
        tail: token,
    },
    Shape {
        prefix: "xoxa-",
        min_tail: 10,
        tail: slack,
    },
    Shape {
        prefix: "xoxb-",
        min_tail: 10,
        tail: slack,
    },
    Shape {
        prefix: "xoxp-",
        min_tail: 10,
        tail: slack,
    },
    Shape {
        prefix: "xoxr-",
        min_tail: 10,
        tail: slack,
    },
    Shape {
        prefix: "xoxs-",
        min_tail: 10,
        tail: slack,
    },
    Shape {
        prefix: "AKIA",
        min_tail: 16,
        tail: upper,
    },
    Shape {
        prefix: "ASIA",
        min_tail: 16,
        tail: upper,
    },
    Shape {
        prefix: "AIza",
        min_tail: 35,
        tail: token,
    },
];

const KEY_BEGIN: &str = "-----BEGIN ";
const KEY_END: &str = "-----END ";
const KEY_TAIL: &str = "PRIVATE KEY-----";
const BEARER: &str = "Bearer ";

/// `text` with every known secret replaced by [`REDACTED`]. Borrowed when
/// nothing matched.
pub fn redact(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut out = String::new();
    // Bytes before `copied` are already in `out`.
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        // A secret starts a word: `disk-...` is not an `sk-` key. Every
        // pattern starts with an ASCII byte, which is a char boundary.
        if !bytes[i].is_ascii() || (i > 0 && token(bytes[i - 1])) {
            i += 1;
            continue;
        }
        let rest = &text[i..];
        let cut = if let Some(len) = private_key(rest) {
            Some((i, i + len))
        } else if let Some(len) = after_prefix(rest, BEARER, 20, bearer) {
            // Keep the scheme so the reader still sees what the header was.
            Some((i + BEARER.len(), i + len))
        } else {
            SHAPES
                .iter()
                .find_map(|s| after_prefix(rest, s.prefix, s.min_tail, s.tail))
                .map(|len| (i, i + len))
        };
        match cut {
            Some((from, to)) => {
                out.push_str(&text[copied..from]);
                out.push_str(REDACTED);
                copied = to;
                i = to;
            }
            None => i += 1,
        }
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

/// The length of the match when `s` starts with `prefix` and at least
/// `min_tail` tail bytes.
fn after_prefix(s: &str, prefix: &str, min_tail: usize, tail: fn(u8) -> bool) -> Option<usize> {
    let rest = s.strip_prefix(prefix)?;
    let n = rest.bytes().take_while(|b| tail(*b)).count();
    (n >= min_tail).then_some(prefix.len() + n)
}

/// The length of a PEM private key block at the start of `s`, to the end of
/// `s` when the block is cut off.
fn private_key(s: &str) -> Option<usize> {
    let header = s.strip_prefix(KEY_BEGIN)?;
    let line = header.find('\n').map_or(header, |n| &header[..n]);
    if !line.trim_end().ends_with(KEY_TAIL) {
        return None;
    }
    let body = KEY_BEGIN.len() + line.len();
    let end = s[body..].find(KEY_END).and_then(|e| {
        let at = body + e;
        s[at..].find(KEY_TAIL).map(|t| at + t + KEY_TAIL.len())
    });
    Some(end.unwrap_or(s.len()))
}

fn redact_in(s: &mut String) {
    if let Cow::Owned(r) = redact(s) {
        *s = r;
    }
}

fn redact_content(c: &mut Content) {
    if let Content::Text(t) = c {
        redact_in(t);
    }
}

/// Redacts the free text an agent reports in a decision, step, edit or
/// progress note. Other kinds are left as they are. Artifacts are stored
/// before the record is written, so the writer redacts their text itself.
pub fn redact_record(record: &mut WorkRecord) {
    match record {
        WorkRecord::Decision {
            so_far,
            question,
            options,
            chosen,
            why,
            ..
        } => {
            for s in [so_far, question, chosen, why] {
                redact_in(s);
            }
            options.iter_mut().for_each(redact_in);
        }
        WorkRecord::Step {
            command, output, ..
        } => {
            redact_in(command);
            redact_content(output);
        }
        WorkRecord::Edit { hunk, .. } => redact_content(hunk),
        WorkRecord::Progress { note, .. } => redact_content(note),
        _ => {}
    }
}

#[cfg(test)]
mod tests;
