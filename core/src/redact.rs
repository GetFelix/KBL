//! Known secret patterns, cut out of what an agent reports before it is
//! written. Tool output and diffs can carry a token the agent read from the
//! environment or a file, and anything on `work.events` stays readable for
//! as long as the item is kept.
//!
//! This is a list of known shapes, not a guarantee: a secret with no
//! recognisable prefix goes through. AWS secret access keys are the common
//! case: 40 base64 characters with nothing to tell them from a hash.

use std::borrow::Cow;

use crate::content::Content;
use crate::work::{FailedCheck, NewPrerequisite, PrSnapshot, RevertCheck, Then, WorkRecord};

/// What a secret is replaced with.
pub const REDACTED: &str = "[redacted]";

/// A prefix, the shortest tail that makes it a secret, the tail's bytes, and
/// a last look at the whole tail.
struct Shape {
    prefix: &'static str,
    min_tail: usize,
    tail: fn(u8) -> bool,
    check: fn(&[u8]) -> bool,
    /// Most tail bytes read. Unbounded only when a tail that fails `check`
    /// fails for every suffix too, which lets the scan skip the rest of it.
    max_tail: usize,
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

fn slack(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

fn bearer(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b)
}

fn base64(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"+/=".contains(&b)
}

fn query_value(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~+/%=".contains(&b)
}

fn any(_: &[u8]) -> bool {
    true
}

/// How many of upper case, lower case, digits and other bytes `s` has.
fn classes(s: &[u8]) -> usize {
    let has = |f: fn(&u8) -> bool| usize::from(s.iter().any(f));
    has(u8::is_ascii_uppercase)
        + has(u8::is_ascii_lowercase)
        + has(u8::is_ascii_digit)
        + has(|b| !b.is_ascii_alphanumeric())
}

/// An access key id is exactly 16 upper case letters or digits after the
/// prefix: `feature/AKIA...EXAMPLE1` is a branch name.
fn aws_id(t: &[u8]) -> bool {
    t.len() == 16
        && t.iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// A provider key has a long random run. `sk-estimator-doc-link-fitted` is a
/// CSS class: short words of one case.
fn random_run(t: &[u8]) -> bool {
    t.split(|b| *b == b'-' || *b == b'_')
        .any(|seg| seg.len() >= 16 && classes(seg) >= 2)
}

/// `Basic` credentials are padded base64 of `user:pass`; `Basic premises` is
/// English.
fn basic(t: &[u8]) -> bool {
    t.len().is_multiple_of(4) && classes(t) >= 2
}

const fn shape(prefix: &'static str, min_tail: usize, tail: fn(u8) -> bool) -> Shape {
    Shape {
        prefix,
        min_tail,
        tail,
        check: any,
        max_tail: usize::MAX,
    }
}

const SHAPES: &[Shape] = &[
    // GitHub tokens and fine-grained tokens.
    shape("ghp_", 36, alnum),
    shape("gho_", 36, alnum),
    shape("ghu_", 36, alnum),
    shape("ghs_", 36, alnum),
    shape("ghr_", 36, alnum),
    shape("github_pat_", 22, word),
    shape("glpat-", 20, token),
    // Model provider keys: `sk-`, `sk-ant-`, `sk-proj-`.
    Shape {
        prefix: "sk-",
        min_tail: 32,
        tail: token,
        check: random_run,
        max_tail: usize::MAX,
    },
    // Stripe secret and restricted keys.
    shape("sk_live_", 24, alnum),
    shape("sk_test_", 24, alnum),
    shape("rk_live_", 24, alnum),
    shape("rk_test_", 24, alnum),
    shape("npm_", 36, alnum),
    // Slack app, bot, user and refresh tokens.
    shape("xapp-", 20, slack),
    shape("xoxa-", 10, slack),
    shape("xoxb-", 10, slack),
    shape("xoxp-", 10, slack),
    shape("xoxr-", 10, slack),
    shape("xoxs-", 10, slack),
    // AWS access key ids; the secret key that goes with one has no prefix.
    Shape {
        prefix: "AKIA",
        min_tail: 16,
        tail: alnum,
        check: aws_id,
        max_tail: 17,
    },
    Shape {
        prefix: "ASIA",
        min_tail: 16,
        tail: alnum,
        check: aws_id,
        max_tail: 17,
    },
    shape("AIza", 35, token),
];

const KEY_BEGIN: &str = "-----BEGIN ";
const KEY_END: &str = "-----END ";
const KEY_TAIL: &str = "PRIVATE KEY-----";
/// Longest label before `PRIVATE KEY-----`, as in `ENCRYPTED ` or `OPENSSH `.
const MAX_KEY_LABEL: usize = 32;
/// Furthest an `END` line may be from its `BEGIN`. An 8192-bit RSA key is
/// under 7 KiB.
const MAX_KEY_BODY: usize = 16 * 1024;
/// Length of a full line of a PEM body.
const PEM_LINE: usize = 64;
/// How far a URL's userinfo is looked for after `://`.
const MAX_USERINFO: usize = 256;

/// Every byte a pattern can start with, `bearer`, `basic` and `token=` in
/// either case. Most bytes are none of these, which keeps the scan cheap.
const FIRST: &[u8] = b"-ABTbegnrstx";

/// `text` with every known secret replaced by [`REDACTED`]. Borrowed when
/// nothing matched. The result is never longer than `text`: a match shorter
/// than the marker becomes as many `*`, so a record under a size cap stays
/// under it. Linear in the length of `text`.
pub fn redact(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut out = String::new();
    // Bytes before `copied` are already in `out`.
    let mut copied = 0;
    let mut scan = Scan::default();
    let mut i = 0;
    while i < bytes.len() {
        // Every pattern starts with an ASCII byte, which is a char boundary.
        let cut = if !bytes[i].is_ascii() {
            None
        } else if bytes[i] == b':' {
            userinfo(bytes, i)
        } else if FIRST.contains(&bytes[i]) && starts_word(bytes, i) {
            secret_at(text, i, &mut scan)
        } else {
            None
        };
        match cut {
            Some((from, to)) => {
                out.push_str(&text[copied..from]);
                if to - from >= REDACTED.len() {
                    out.push_str(REDACTED);
                } else {
                    out.extend(std::iter::repeat_n('*', to - from));
                }
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

/// Whether a secret may start at `i`. `disk-...` is not an `sk-` key, but a
/// `-` (a removed diff line), `_` (`TOKEN_ghp_...`) or a JSON escape
/// (`\nghp_...`, `"ghp_...`) may come before one.
fn starts_word(bytes: &[u8], i: usize) -> bool {
    let Some(prev) = i.checked_sub(1).map(|p| bytes[p]) else {
        return true;
    };
    if !prev.is_ascii_alphanumeric() {
        return true;
    }
    let escape = i >= 2 && bytes[i - 2] == b'\\' && b"bfnrt".contains(&prev);
    let unicode = i >= 6
        && &bytes[i - 6..i - 4] == b"\\u"
        && bytes[i - 4..i].iter().all(u8::is_ascii_hexdigit);
    escape || unicode
}

/// Where a scan is. A pattern whose tail ran to `dead[k]` and failed also
/// fails for a later prefix that ends by then, since that one reads a suffix
/// of the same tail. Skipping those keeps `sk-sk-sk-...` linear.
struct Scan {
    ends: NextEnd,
    dead: [usize; SHAPES.len() + 2],
}

impl Default for Scan {
    fn default() -> Self {
        Self {
            ends: NextEnd::default(),
            dead: [0; SHAPES.len() + 2],
        }
    }
}

const DEAD_TOKEN: usize = SHAPES.len();
const DEAD_JWT: usize = SHAPES.len() + 1;

/// A name kept in the text, the shortest value after it, the value's bytes,
/// a last look at the value, and the `dead` slot when the name can run into
/// itself.
type Named = (
    &'static str,
    usize,
    fn(u8) -> bool,
    fn(&[u8]) -> bool,
    Option<usize>,
);

/// The span to cut for a secret starting at `i`.
fn secret_at(text: &str, i: usize, scan: &mut Scan) -> Option<(usize, usize)> {
    let rest = &text.as_bytes()[i..];
    if let Some(len) = private_key(text, i, &mut scan.ends) {
        return Some((i, i + len));
    }
    // Keep the scheme or the parameter name so the reader still sees what it was.
    // `bearer ` and `basic ` end in a space their tails cannot hold, so only
    // `token=` can run into itself.
    let named: [Named; 3] = [
        ("bearer ", 20, bearer, any, None),
        ("basic ", 12, base64, basic, None),
        ("token=", 10, query_value, any, Some(DEAD_TOKEN)),
    ];
    for (name, min, class, check, dead) in named {
        if dead.is_some_and(|d| i + name.len() <= scan.dead[d]) {
            continue;
        }
        match after_name(rest, name, min, class, check) {
            Some(Ok(n)) => return Some((i + name.len(), i + name.len() + n)),
            Some(Err(n)) => {
                if let Some(d) = dead {
                    scan.dead[d] = i + name.len() + n;
                }
            }
            None => {}
        }
    }
    if i + 3 > scan.dead[DEAD_JWT] {
        match jwt(rest) {
            Some(Ok(len)) => return Some((i, i + len)),
            Some(Err(head)) => scan.dead[DEAD_JWT] = i + head,
            None => {}
        }
    }
    let len = SHAPES.iter().enumerate().find_map(|(k, s)| {
        if i + s.prefix.len() <= scan.dead[k] {
            return None;
        }
        let tail = rest.strip_prefix(s.prefix.as_bytes())?;
        let n = tail
            .iter()
            .take(s.max_tail)
            .take_while(|b| (s.tail)(**b))
            .count();
        let hit = n >= s.min_tail && (s.check)(&tail[..n]);
        if !hit && s.max_tail == usize::MAX {
            scan.dead[k] = i + s.prefix.len() + n;
        }
        hit.then_some(s.prefix.len() + n)
    })?;
    // Take the rest of the run, so a second token stuck to the first
    // (`ghp_...ghp_...`) goes with it.
    let more = rest[len..].iter().take_while(|b| token(**b)).count();
    Some((i, i + len + more))
}

/// After `name` (matched without case), the length of the value when it is
/// at least `min` bytes of `class`, or of the run that is not.
fn after_name(
    s: &[u8],
    name: &str,
    min: usize,
    class: fn(u8) -> bool,
    check: fn(&[u8]) -> bool,
) -> Option<Result<usize, usize>> {
    if !s.get(..name.len())?.eq_ignore_ascii_case(name.as_bytes()) {
        return None;
    }
    let tail = &s[name.len()..];
    let n = tail.iter().take_while(|b| class(**b)).count();
    Some(if n >= min && check(&tail[..n]) {
        Ok(n)
    } else {
        Err(n)
    })
}

/// The length of a JWT at the start of `s`: three base64url segments, the
/// first an encoded `{"`. Not one: the length of the first segment, which a
/// later start inside it would read the rest of and fail the same way.
fn jwt(s: &[u8]) -> Option<Result<usize, usize>> {
    if !s.starts_with(b"eyJ") {
        return None;
    }
    let run = |from: usize| s[from..].iter().take_while(|c| token(**c)).count();
    let head = run(0);
    if head < 10 || s.get(head) != Some(&b'.') {
        return Some(Err(head));
    }
    let body = run(head + 1);
    let sig_at = head + 1 + body;
    if body < 2 || s.get(sig_at) != Some(&b'.') {
        return Some(Err(head));
    }
    let sig = run(sig_at + 1);
    Some(if sig >= 16 {
        Ok(sig_at + 1 + sig)
    } else {
        Err(head)
    })
}

/// The userinfo of `scheme://user:pass@`, for the `:` at `i` that starts
/// `://`. The user goes too: it is often the token itself.
fn userinfo(b: &[u8], i: usize) -> Option<(usize, usize)> {
    if !b[i..].starts_with(b"://") || i == 0 || !b[i - 1].is_ascii_alphanumeric() {
        return None;
    }
    let from = i + 3;
    let mut colon = None;
    for (j, &c) in b.iter().enumerate().skip(from).take(MAX_USERINFO) {
        match c {
            b'@' => return (j > colon? + 1).then_some((from, j)),
            b':' if colon.is_none() => colon = Some(j),
            b'/' | b'?' | b'#' | b'"' | b'\'' | b'<' | b'>' | b'\\' => return None,
            c if c.is_ascii_whitespace() => return None,
            _ => {}
        }
    }
    None
}

/// The first `-----END ` at or after a position. Positions asked for only
/// grow, so each search starts past the last one's result and the total
/// work stays linear however many `BEGIN` lines the text has.
#[derive(Default)]
struct NextEnd {
    /// `(from, found)`: the search from `from` found `found`.
    last: Option<(usize, Option<usize>)>,
}

impl NextEnd {
    fn from(&mut self, text: &str, p: usize) -> Option<usize> {
        if let Some((from, found)) = self.last
            && from <= p
            && found.is_none_or(|f| f >= p)
        {
            return found;
        }
        let found = text[p..].find(KEY_END).map(|e| p + e);
        self.last = Some((p, found));
        found
    }
}

/// The length of a PEM private key block at `i`. Without an `END` of the
/// same type within [`MAX_KEY_BODY`], only the header and the base64 lines
/// right after it are cut: that covers output that was cut off, and leaves
/// the rest of a grep line or diff hunk that only names the header.
fn private_key(text: &str, i: usize, ends: &mut NextEnd) -> Option<usize> {
    let header = text.as_bytes()[i..].strip_prefix(KEY_BEGIN.as_bytes())?;
    // A key label is short; looking further would make a long line of
    // `-----BEGIN ` quadratic.
    let near = &header[..header.len().min(MAX_KEY_LABEL + KEY_TAIL.len())];
    let label = near
        .windows(KEY_TAIL.len())
        .position(|w| w == KEY_TAIL.as_bytes())?;
    let label = &near[..label];
    if label.iter().any(|b| *b == b'\n' || *b == b'\\') {
        return None;
    }
    let body = i + KEY_BEGIN.len() + label.len() + KEY_TAIL.len();
    if let Some(e) = ends.from(text, body).filter(|e| e - body <= MAX_KEY_BODY) {
        let tail = &text.as_bytes()[e + KEY_END.len()..];
        if tail.starts_with(label) && tail[label.len()..].starts_with(KEY_TAIL.as_bytes()) {
            return Some(e + KEY_END.len() + label.len() + KEY_TAIL.len() - i);
        }
    }
    Some(body + pem_lines(&text.as_bytes()[body..]) - i)
}

/// How many bytes of `s` are line breaks (real or JSON-escaped) each followed
/// by a line of base64: full lines, then at most one short last line.
fn pem_lines(s: &[u8]) -> usize {
    let mut at = 0;
    while at < MAX_KEY_BODY {
        let Some(br) = line_break(&s[at..]) else {
            break;
        };
        let line = s[at + br..]
            .iter()
            .take(PEM_LINE + 2)
            .take_while(|b| base64(**b))
            .count();
        let next = at + br + line;
        let ends_line = next == s.len() || line_break(&s[next..]).is_some();
        // A diff marker makes a full line one longer.
        if line == 0 || line > PEM_LINE + 1 || !ends_line {
            break;
        }
        at = next;
        if line < PEM_LINE {
            break;
        }
    }
    at
}

fn line_break(s: &[u8]) -> Option<usize> {
    ["\r\n", "\n", "\\r\\n", "\\n"]
        .iter()
        .find(|b| s.starts_with(b.as_bytes()))
        .map(|b| b.len())
}

fn redact_in(s: &mut String) {
    if let Cow::Owned(r) = redact(s) {
        *s = r;
    }
}

fn redact_opt(s: &mut Option<String>) {
    s.iter_mut().for_each(redact_in);
}

fn redact_all(v: &mut [String]) {
    v.iter_mut().for_each(redact_in);
}

fn redact_content(c: &mut Content) {
    if let Content::Text(t) = c {
        redact_in(t);
    }
}

/// Redacts every free-text field of a record. Artifacts are stored before
/// the record is written, so the writer redacts their text itself.
///
/// Left as they are: item ids, keys, attempts, offsets, costs, tiers and
/// enums, which the tracker checks against fixed shapes; evidence (a stream
/// and an offset); sources, upstream ids and targets, which name where an
/// item comes from; a review's tier and `pr`, a pull request reference and
/// a refusal's `op`, which only the tracker writes.
///
/// The match has no wildcard, so a new kind does not compile until its
/// fields are sorted into one list or the other.
pub fn redact_record(record: &mut WorkRecord) {
    match record {
        WorkRecord::Imported { title, .. } | WorkRecord::Proposed { title, .. } => redact_in(title),
        WorkRecord::Triaged { why, split, .. } => {
            redact_in(why);
            redact_all(split);
        }
        WorkRecord::Prerequisite { new, .. } => {
            if let Some(NewPrerequisite { title, why, .. }) = new {
                redact_in(title);
                redact_content(why);
            }
        }
        WorkRecord::Shipped { release, .. } => redact_in(release),
        WorkRecord::Context { text, .. } | WorkRecord::HumanInput { text, .. } => {
            redact_content(text)
        }
        WorkRecord::Progress {
            note, branch, pr, ..
        } => {
            redact_content(note);
            redact_opt(branch);
            redact_opt(pr);
        }
        WorkRecord::Released { reason, .. } => redact_opt(reason),
        WorkRecord::GaveUp { reason, .. } => redact_in(reason),
        WorkRecord::Question {
            text,
            options,
            default,
            ..
        } => {
            redact_content(text);
            redact_all(options);
            redact_opt(default);
        }
        WorkRecord::Answer {
            text, chosen, then, ..
        } => {
            redact_content(text);
            redact_opt(chosen);
            match then {
                Some(Then::Prerequisite { title, .. }) => redact_in(title),
                Some(Then::Resume) | None => {}
            }
        }
        WorkRecord::Decision {
            so_far,
            question,
            options,
            chosen,
            why,
            commit,
            revert_check,
            ..
        } => {
            for s in [so_far, question, chosen, why, commit] {
                redact_in(s);
            }
            redact_all(options);
            if let Some(RevertCheck { test, .. }) = revert_check {
                redact_in(test);
            }
        }
        WorkRecord::Finding {
            file,
            title,
            scenario,
            ..
        } => {
            for s in [file, title, scenario] {
                redact_in(s);
            }
        }
        WorkRecord::ReviewVerdict { why, .. } => redact_in(why),
        WorkRecord::Step {
            command, output, ..
        } => {
            redact_in(command);
            redact_content(output);
        }
        WorkRecord::Edit { file, hunk, .. } => {
            redact_in(file);
            redact_content(hunk);
        }
        WorkRecord::Updated { title, .. } | WorkRecord::Edited { title, .. } => redact_opt(title),
        WorkRecord::PrStatus {
            status: PrSnapshot { failing, .. },
            ..
        } => {
            if let Some(FailedCheck { name, url }) = failing {
                redact_in(name);
                redact_in(url);
            }
        }
        WorkRecord::RulesSet { text, checks, .. } => {
            redact_opt(text);
            if let Some(c) = checks {
                redact_all(c);
            }
        }
        WorkRecord::Accepted { .. }
        | WorkRecord::Linked { .. }
        | WorkRecord::Unlinked { .. }
        | WorkRecord::Claimed { .. }
        | WorkRecord::Done { .. }
        | WorkRecord::ForkedFrom { .. }
        | WorkRecord::Closed { .. }
        | WorkRecord::Rejected { .. }
        | WorkRecord::LeftScope { .. }
        | WorkRecord::ReturnedToScope { .. }
        | WorkRecord::Reopened { .. }
        | WorkRecord::AttemptClosed { .. }
        | WorkRecord::TierChanged { .. }
        | WorkRecord::BudgetSet { .. }
        | WorkRecord::SourceStalled { .. }
        | WorkRecord::FormatRaised { .. }
        | WorkRecord::Refused { .. }
        | WorkRecord::Unknown => {}
    }
}

#[cfg(test)]
mod tests;
