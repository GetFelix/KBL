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
/// prefix: `feature/AKIA...EXAMPLE1` is a branch name. Two ids stuck
/// together are 36 bytes: the first's 16, then a whole second id.
fn aws_id(t: &[u8]) -> bool {
    let id = |t: &[u8]| {
        t.len() == 16
            && t.iter()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    };
    id(t)
        || (t.len() == 36
            && id(&t[..16])
            && (t[16..].starts_with(b"AKIA") || t[16..].starts_with(b"ASIA"))
            && id(&t[20..]))
}

/// A generated credential has a long run of letters and digits mixed:
/// `sk-add-TokenValidatorForUsers-to-the-api` is a branch name and
/// `sk-estimator-doc-link-fitted` a CSS class. Split at every other byte,
/// so a run in a suffix that starts after one is a run of the whole.
fn random_run(t: &[u8]) -> bool {
    t.split(|b| !b.is_ascii_alphanumeric()).any(|seg| {
        seg.len() >= 16
            && seg.iter().any(u8::is_ascii_digit)
            && seg.iter().any(u8::is_ascii_alphabetic)
    })
}

/// A `random_run` over a URL-encoded value, where `%2B` is one byte of the
/// run and not the end of it.
fn encoded_random_run(t: &[u8]) -> bool {
    let (mut len, mut digit, mut alpha, mut at) = (0, false, false, 0);
    while at < t.len() {
        let b = t[at];
        let pct = b == b'%'
            && t.get(at + 1..at + 3)
                .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit));
        if pct {
            at += 3;
            len += 1;
        } else if b.is_ascii_alphanumeric() {
            at += 1;
            len += 1;
            digit |= b.is_ascii_digit();
            alpha |= b.is_ascii_alphabetic();
        } else {
            at += 1;
            (len, digit, alpha) = (0, false, false);
            continue;
        }
        if len >= 16 && digit && alpha {
            return true;
        }
    }
    false
}

/// Slack tokens are a numeric id, perhaps more ids, then a secret, joined
/// by `-`: `xoxb-<team>-<bot>-<secret>`, `xoxb-<team>-<secret>`,
/// `xapp-<n>-<app>-<ts>-<secret>`, `xoxe-1-<secret>`. The ids have no lower
/// case, and the secret is the first segment that does, or the last one when
/// none does: at least 12 bytes of mixed classes, so `xoxb-slackbot-tools` and
/// `xoxb-2-migration-of-legacy-tokens` are names. Anything after the secret
/// (`...-prod`) is cut with it.
fn slack_token(t: &[u8]) -> bool {
    let mut segs = t.split(|b| *b == b'-');
    if !segs
        .next()
        .is_some_and(|s| !s.is_empty() && s.iter().all(u8::is_ascii_digit))
    {
        return false;
    }
    let mut last: &[u8] = &[];
    for seg in segs {
        if seg.iter().any(u8::is_ascii_lowercase) {
            return seg.len() >= 12 && classes(seg) >= 2;
        }
        if seg.is_empty() {
            return false;
        }
        last = seg;
    }
    // A secret with no lower case is the last segment.
    last.len() >= 12 && classes(last) >= 2
}

/// Whether the bytes before a scheme end in an `Authorization` header name
/// and a `:`, `=` or `,`, as in `Authorization: `, `"authorization": "`,
/// `headers["Authorization"] = "` or `.header(AUTHORIZATION, "`. A `,` needs
/// a quote too: `authentication, authorization, basic ...` is prose.
fn authorization(before: &[u8]) -> bool {
    let w = &before[before.len().saturating_sub(48)..];
    let gap = w
        .iter()
        .rev()
        .take_while(|b| b.is_ascii_whitespace() || b"\"'`[]():=,>\\".contains(b))
        .count();
    let (s, gap) = w.split_at(w.len() - gap);
    let name = b"authorization";
    let quoted = gap.iter().any(|b| b"\"'`".contains(b));
    (gap.iter().any(|b| b":=".contains(b)) || (quoted && gap.contains(&b',')))
        && s.len() >= name.len()
        && s[s.len() - name.len()..].eq_ignore_ascii_case(name)
}

/// `Basic` credentials are base64 of `user:pass`, padded to a multiple of 4
/// with at most two `=`. After an `Authorization` header that is enough;
/// elsewhere a digit, `+`, `/` or `=` must show it is not a word, so
/// `Basic Requirements` and `basic HttpResponse` stay.
fn basic(before: &[u8], t: &[u8], _: &[u8]) -> bool {
    let pad = t.iter().rev().take_while(|b| **b == b'=').count();
    let shape = t.len().is_multiple_of(4)
        && pad <= 2
        && !t[..t.len() - pad].contains(&b'=')
        && classes(t) >= 2;
    shape && (authorization(before) || t.iter().any(|b| b.is_ascii_digit() || b"+/=".contains(b)))
}

/// Outside an `Authorization` header a bearer token needs a digit:
/// `Bearer authentication/authorization-flow` is prose. Inside one, a value
/// with no digit is still not a token when it is a call or a path
/// (`bearer tokenProvider.get()`) or words joined by `-`
/// (`Bearer your-access-token-here`).
fn bearer_value(before: &[u8], t: &[u8], after: &[u8]) -> bool {
    if !t.iter().any(u8::is_ascii_alphabetic) {
        return false;
    }
    if t.iter().any(u8::is_ascii_digit) {
        return true;
    }
    // A word is lower case, perhaps capitalised, or all upper case.
    let word = |w: &[u8]| {
        w.iter().all(u8::is_ascii_alphabetic)
            && (w[1..].iter().all(u8::is_ascii_lowercase) || w.iter().all(u8::is_ascii_uppercase))
    };
    let words = t.contains(&b'-') && t.split(|b| *b == b'-').all(|w| !w.is_empty() && word(w));
    authorization(before) && !after.starts_with(b"(") && !dotted_path(t) && !words
}

/// Whether a `token=` at the end of `before` is a URL query parameter, as in
/// `?token=` or `&access_token=`.
fn in_query(before: &[u8]) -> bool {
    let name = before
        .iter()
        .rev()
        .take(64)
        .take_while(|b| word(**b))
        .count();
    before.len() > name && matches!(before[before.len() - name - 1], b'?' | b'&')
}

/// `a.b_c.d`: a field or module path.
fn dotted_path(t: &[u8]) -> bool {
    t.contains(&b'.')
        && t.split(|b| *b == b'.').all(|seg| {
            seg.first()
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
                && seg.iter().all(|b| word(*b))
        })
}

/// In a URL query any value is cut. Elsewhere `token=` is as often code
/// (`Client(token=os.environ.get("GH"))`, `next_token=settings.API_TOKEN`),
/// so only a value with a long random run that is not a call or a path is.
fn token_value(before: &[u8], t: &[u8], after: &[u8]) -> bool {
    in_query(before) || (!after.starts_with(b"(") && !dotted_path(t) && random_run(t))
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

/// Longest Slack token read; real ones are under 100 bytes.
const MAX_SLACK: usize = 128;

const fn slack_shape(prefix: &'static str, min_tail: usize) -> Shape {
    Shape {
        prefix,
        min_tail,
        tail: slack,
        check: slack_token,
        max_tail: MAX_SLACK,
    }
}

/// An id and a second whole id stuck to it; one more byte than that is not.
const MAX_AWS: usize = 36;

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
    // Slack app, bot, user and refresh tokens. The check reads the start of
    // the tail, so a later prefix inside a failed one can still match and the
    // tail is bounded instead.
    // Rotated tokens first, so the cut starts at `xoxe.`.
    slack_shape("xoxe.xoxb-", 10),
    slack_shape("xoxe.xoxp-", 10),
    slack_shape("xoxe-", 10),
    slack_shape("xapp-", 20),
    slack_shape("xoxa-", 10),
    slack_shape("xoxb-", 10),
    slack_shape("xoxc-", 10),
    slack_shape("xoxp-", 10),
    slack_shape("xoxr-", 10),
    slack_shape("xoxs-", 10),
    // AWS access key ids; the secret key that goes with one has no prefix.
    Shape {
        prefix: "AKIA",
        min_tail: 16,
        tail: alnum,
        check: aws_id,
        max_tail: MAX_AWS,
    },
    Shape {
        prefix: "ASIA",
        min_tail: 16,
        tail: alnum,
        check: aws_id,
        max_tail: MAX_AWS,
    },
    // Slack `d` cookie: URL-encoded base64, so `%2B` splits the runs.
    Shape {
        prefix: "xoxd-",
        min_tail: 32,
        tail: query_value,
        check: encoded_random_run,
        max_tail: usize::MAX,
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
/// Longest indentation or quote prefix read on a line of a PEM body. Deeper
/// than any YAML or JSON nesting seen in practice; a line indented further
/// is not read as part of a key with no `END`.
const MAX_PEM_PREFIX: usize = 128;
/// Shortest base64 run a body between `BEGIN` and `END` must have, so prose
/// between two marker mentions is not a key.
const MIN_PEM_RUN: usize = 32;
/// Length of a full line of a PEM body.
const PEM_LINE: usize = 64;
/// Longest full line of a body with no `END`; OpenSSH keys use 70.
const MAX_PEM_LINE_FULL: usize = 70;
/// Longest body line taken inside a block.
const MAX_PEM_LINE: usize = 80;
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
    cut(text, true)
}

/// `text` with only the userinfo of a URL cut, for a value whose content is
/// a key: a pull request reference or a branch name, which a token-shaped
/// word in a repo name must not change.
fn redact_userinfo(text: &str) -> Cow<'_, str> {
    cut(text, false)
}

/// [`redact`], or with `shapes` false only URL userinfo.
fn cut(text: &str, shapes: bool) -> Cow<'_, str> {
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
        } else if shapes && FIRST.contains(&bytes[i]) && starts_word(bytes, i) {
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

/// A last look at a value: the text before the name, the value, and the
/// text after the value.
type NamedCheck = fn(&[u8], &[u8], &[u8]) -> bool;

/// A name kept in the text, the shortest value after it, the value's bytes,
/// a last look at the value, and the `dead` slot when the name can run into
/// itself.
type Named = (
    &'static str,
    usize,
    fn(u8) -> bool,
    NamedCheck,
    Option<usize>,
);

/// The span to cut for a secret starting at `i`.
fn secret_at(text: &str, i: usize, scan: &mut Scan) -> Option<(usize, usize)> {
    let (before, rest) = text.as_bytes().split_at(i);
    if let Some(len) = private_key(text, i, &mut scan.ends) {
        return Some((i, i + len));
    }
    // Keep the scheme or the parameter name so the reader still sees what it was.
    // `bearer ` and `basic ` end in a space their tails cannot hold, so only
    // `token=` can run into itself. Its check holds for a value if it holds
    // for any suffix that starts after a `token=` inside it, which is what
    // lets a failed value mark the rest dead.
    let named: [Named; 3] = [
        ("bearer ", 20, bearer, bearer_value, None),
        ("basic ", 12, base64, basic, None),
        ("token=", 10, query_value, token_value, Some(DEAD_TOKEN)),
    ];
    for (name, min, class, check, dead) in named {
        if dead.is_some_and(|d| i + name.len() <= scan.dead[d]) {
            continue;
        }
        match after_name(before, rest, name, min, class, check) {
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
    before: &[u8],
    s: &[u8],
    name: &str,
    min: usize,
    class: fn(u8) -> bool,
    check: NamedCheck,
) -> Option<Result<usize, usize>> {
    if !s.get(..name.len())?.eq_ignore_ascii_case(name.as_bytes()) {
        return None;
    }
    let tail = &s[name.len()..];
    let n = tail.iter().take_while(|b| class(**b)).count();
    Some(if n >= min && check(before, &tail[..n], &tail[n..]) {
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
    if sig < 16 {
        return Some(Err(head));
    }
    let mut end = sig_at + 1 + sig;
    let mut last = sig_at + 1;
    // A second JWT stuck to the first's signature has its header inside the
    // signature run, so its body and signature follow after a `.`.
    while s.get(end) == Some(&b'.') && s[last + 16..end].windows(3).any(|w| w == b"eyJ") {
        let body = run(end + 1);
        let next = end + 1 + body;
        let sig = run(next + 1);
        if s.get(next) != Some(&b'.') || sig < 16 {
            break;
        }
        last = next + 1;
        end = last + sig;
    }
    Some(Ok(end))
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
/// same type within [`MAX_KEY_BODY`] and only base64 lines between them,
/// only the header and the base64 lines right after it are cut: that covers
/// output that was cut off, and leaves the rest of a grep line, a diff hunk
/// or code that only names the markers.
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
        if tail.starts_with(label)
            && tail[label.len()..].starts_with(KEY_TAIL.as_bytes())
            && pem_body(&text.as_bytes()[body..e])
        {
            return Some(e + KEY_END.len() + label.len() + KEY_TAIL.len() - i);
        }
    }
    Some(body + pem_lines(&text.as_bytes()[body..]) - i)
}

/// Whether `s`, from the end of a key's header to its `END`, is runs of
/// base64 split by line breaks or spaces (a key pasted on one line). A line
/// may start with indentation, a diff marker or a `>` quote. Stops at the
/// first byte that is none of these, so a `BEGIN` inside a failed body is
/// not read twice. At least one run must be [`MIN_PEM_RUN`] long.
fn pem_body(s: &[u8]) -> bool {
    let mut at = 0;
    let mut long = false;
    let mut line_start = true;
    while at < s.len() {
        if let Some(br) = line_break(&s[at..]) {
            at += br;
            line_start = true;
            continue;
        }
        // `+` is base64 already.
        if matches!(s[at], b' ' | b'\t') || (line_start && matches!(s[at], b'-' | b'>')) {
            at += 1;
            continue;
        }
        let n = s[at..]
            .iter()
            .take(MAX_PEM_LINE + 1)
            .take_while(|b| base64(**b))
            .count();
        if n == 0 || n > MAX_PEM_LINE {
            return false;
        }
        at += n;
        line_start = false;
        long |= n >= MIN_PEM_RUN;
    }
    long
}

/// How many bytes of `s` are base64 lines after a key's header, each after a
/// line break (real or JSON-escaped) and perhaps a line prefix: full lines,
/// then at most one short last line. A key pasted on one line has its full
/// lines split by spaces instead. The first line must be full unless an
/// `END` follows it, so a short reply or diff line after a header is left.
/// A line may also end at a closing quote, as in `K="-----BEGIN ... AB12=="`.
fn pem_lines(s: &[u8]) -> usize {
    let mut at = 0;
    let mut full = false;
    while at < MAX_KEY_BODY {
        let (mut gap, _) = pem_gap(&s[at..]);
        if gap == 0 {
            break;
        }
        // One blank line is allowed between the header and the body.
        if at == 0 && line_break(&s[gap..]).is_some() {
            gap += pem_gap(&s[gap..]).0;
        }
        let start = at + gap;
        let line = s[start..]
            .iter()
            .take(MAX_PEM_LINE_FULL + 2)
            .take_while(|b| base64(**b))
            .count();
        let next = start + line;
        // A diff marker makes a full line one longer.
        if line == 0 || line > MAX_PEM_LINE_FULL + 1 {
            break;
        }
        // A full line may also end at a space, as in a key pasted on one line.
        let (n, br) = pem_gap(&s[next..]);
        let after = &s[next..];
        let closes =
            after.first().is_some_and(|b| b"\"'`".contains(b)) || after.starts_with(b"\\\"");
        let padded = s[next - 1] == b'=' && n > 0;
        let ends_line =
            br || closes || next + n == s.len() || (n > 0 && line >= PEM_LINE) || (full && padded);
        // A short first line is a key's when it is long and mixed enough that
        // no word or hash is: `head -c 40 key.pem`, or a 48-column wrap.
        let first_short =
            !full && line < PEM_LINE && !(line >= MIN_PEM_RUN && classes(&s[start..next]) >= 3);
        // The gap's line prefix takes the dashes of an `END` line.
        let end_follows = br && n >= 5 && s[next + n - 5..].starts_with(KEY_END.as_bytes());
        if !ends_line || (first_short && !end_follows) {
            break;
        }
        at = next;
        if line < PEM_LINE {
            break;
        }
        full = true;
    }
    at
}

/// The gap before the next run of a PEM body: spaces or tabs, then perhaps
/// a line break and a line prefix (indentation, a `>` quote, a `-` diff
/// marker, or a `+` one followed by a space). Whether it held a line break.
fn pem_gap(s: &[u8]) -> (usize, bool) {
    let blank = |b: &u8| matches!(*b, b' ' | b'\t');
    let spaces = s
        .iter()
        .take(MAX_PEM_PREFIX)
        .take_while(|b| blank(b))
        .count();
    let Some(br) = line_break(&s[spaces..]) else {
        return (spaces, false);
    };
    let from = spaces + br;
    let prefix = s[from..]
        .iter()
        .enumerate()
        .take(MAX_PEM_PREFIX)
        .take_while(|&(k, b)| {
            blank(b)
                || matches!(*b, b'>' | b'-')
                || (*b == b'+' && s.get(from + k + 1).is_some_and(blank))
        })
        .count();
    (from + prefix, true)
}

fn line_break(s: &[u8]) -> Option<usize> {
    let one = escaped_break(s)?;
    // `\n\` then a real newline continues a Rust string: one break, not two.
    // Only after an escape, so a blank line stays a blank line.
    if s[0] == b'\\'
        && s.get(one) == Some(&b'\\')
        && let Some(more) = escaped_break(&s[one..])
    {
        return Some(one + more);
    }
    Some(one)
}

/// A line break as text carries it: a real one, `\n` or `\r\n` with any
/// depth of escaping (JSON inside JSON), or backslash then a real
/// newline.
fn escaped_break(s: &[u8]) -> Option<usize> {
    if s.starts_with(b"\r\n") {
        return Some(2);
    }
    if s.starts_with(b"\n") {
        return Some(1);
    }
    let slashes = |s: &[u8]| s.iter().take_while(|b| **b == b'\\').count();
    let bs = slashes(s);
    if bs == 0 {
        return None;
    }
    let rest = &s[bs..];
    if rest.starts_with(b"\r\n") {
        return Some(bs + 2);
    }
    if rest.starts_with(b"\n") || rest.first() == Some(&b'n') {
        return Some(bs + 1);
    }
    if rest.first() == Some(&b'r') {
        let more = slashes(&rest[1..]);
        if more > 0 && rest.get(1 + more) == Some(&b'n') {
            return Some(bs + 1 + more + 1);
        }
    }
    None
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
/// a refusal's `op`, which only the tracker writes. A progress record's
/// `pr` and `branch` are keys too (the fold matches a pull request by
/// `pr`), so only URL userinfo is cut from them: `acme/xoxb-slackbot-tools#7`
/// is a repo, not a token.
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
            for s in [branch, pr].into_iter().flatten() {
                if let Cow::Owned(r) = redact_userinfo(s) {
                    *s = r;
                }
            }
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
        | WorkRecord::Flagged { .. }
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
