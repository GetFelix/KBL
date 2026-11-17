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
/// together are the first's 16 bytes, perhaps one joining byte, then a whole
/// second id; the rest of the run goes with them.
fn aws_id(t: &[u8]) -> bool {
    let id = |t: &[u8]| {
        t.len() == 16
            && t.iter()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    };
    let second = |r: &[u8]| {
        r.len() >= 20 && (r.starts_with(b"AKIA") || r.starts_with(b"ASIA")) && id(&r[4..20])
    };
    id(t)
        || (t.len() >= 36
            && id(&t[..16])
            && (second(&t[16..]) || (t.len() == MAX_AWS && second(&t[17..]))))
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

/// An id, one joining byte and a second whole id.
const MAX_AWS: usize = 37;

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

/// A key's markers. A space in a marker or a key tail also matches `+` or
/// `%20`, as in a URL-encoded key.
const KEY_BEGIN: &str = "-----BEGIN ";
const KEY_END: &str = "-----END ";
/// What ends a private key's header, after a label such as `RSA ` or `PGP `.
const KEY_TAILS: [&str; 2] = ["PRIVATE KEY-----", "PRIVATE KEY BLOCK-----"];
/// Longest label before a key tail, as in `ENCRYPTED ` or `OPENSSH `.
const MAX_KEY_LABEL: usize = 32;
/// Longest `Name: value` line read after a key header (`Proc-Type:`,
/// `DEK-Info:`, or a PGP `Comment:`).
const MAX_ARMOR_LINE: usize = 256;
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
/// `-` (a removed diff line), `_` (`TOKEN_ghp_...`), a JSON escape
/// (`\nghp_...`, `"ghp_...`) or a URL-encoded byte (`%20ghp_...`,
/// `d%3Dxoxd-...`) may come before one.
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
    let encoded =
        i >= 3 && bytes[i - 3] == b'%' && bytes[i - 2..i].iter().all(u8::is_ascii_hexdigit);
    escape || unicode || encoded
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

/// What every `END` line starts with, however its space is encoded.
const KEY_END_MARK: &str = "-----END";

/// The first `-----END` at or after a position. Positions asked for only
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
        let found = text[p..].find(KEY_END_MARK).map(|e| p + e);
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
    let bytes = text.as_bytes();
    let (label, header) = key_header(&bytes[i..])?;
    let body = i + header;
    let head = armor(&bytes[body..]);
    let from = body + head;
    if let Some(e) = ends.from(text, from).filter(|e| e - from <= MAX_KEY_BODY)
        && let Some(end) = key_end(&bytes[e..], label)
        && pem_body(&bytes[from..e])
    {
        return Some(e + end - i);
    }
    Some(body + pem_lines(&bytes[body..], head) - i)
}

/// The label (`RSA `, `PGP `, as written) and the length of the private
/// key header at the start of `s`.
fn key_header(s: &[u8]) -> Option<(&[u8], usize)> {
    let begin = spaced(s, KEY_BEGIN.as_bytes())?;
    let header = &s[begin..];
    // A key label is short; looking further would make a long line of
    // `-----BEGIN ` quadratic.
    let (label, tail) = KEY_TAILS.iter().find_map(|t| {
        (0..=header.len().min(MAX_KEY_LABEL))
            .find_map(|at| Some((at, spaced(&header[at..], t.as_bytes())?)))
    })?;
    let label = &header[..label];
    if label.iter().any(|b| *b == b'\n' || *b == b'\\') {
        return None;
    }
    Some((label, begin + label.len() + tail))
}

/// The length of an `END` line for a key with `label` at the start of `s`.
fn key_end(s: &[u8], label: &[u8]) -> Option<usize> {
    let mut at = spaced(s, KEY_END.as_bytes())?;
    at += spaced(&s[at..], label)?;
    let tail = KEY_TAILS
        .iter()
        .find_map(|t| spaced(&s[at..], t.as_bytes()))?;
    Some(at + tail)
}

/// A space as a URL carries it: itself, `+` or `%20`.
fn space(s: &[u8]) -> Option<usize> {
    match s.first()? {
        b' ' | b'+' => Some(1),
        b'%' if s.get(1..3) == Some(b"20") => Some(3),
        _ => None,
    }
}

/// The length of `pat` at the start of `s`, where a space in either may be
/// written any way [`space`] reads.
fn spaced(s: &[u8], pat: &[u8]) -> Option<usize> {
    let (mut at, mut p) = (0, 0);
    while p < pat.len() {
        if let Some(n) = space(&pat[p..]) {
            p += n;
            at += space(s.get(at..)?)?;
        } else if s.get(at) == Some(&pat[p]) {
            p += 1;
            at += 1;
        } else {
            return None;
        }
    }
    Some(at)
}

/// Whether `s`, from the end of a key's armor to its `END`, is runs of
/// base64 split by line breaks or spaces (a key pasted on one line). A line
/// may start with indentation, a diff marker or a `>` quote. Stops at the
/// first byte that is none of these, so a `BEGIN` inside a failed body is
/// not read twice. At least one run must be [`MIN_PEM_RUN`] long. A run may
/// be the whole body, as in a key with its line breaks stripped.
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
        if let Some(n) = body_space(&s[at..]) {
            at += n;
            continue;
        }
        if line_start && matches!(s[at], b'-' | b'>') {
            at += 1;
            continue;
        }
        let (bytes, n) = base64_run(&s[at..], usize::MAX);
        if n == 0 {
            return false;
        }
        at += bytes;
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
/// A run stuck to the header is a body with its line breaks stripped, and
/// goes whole when it is longer than a line. `head` is the armor's length.
fn pem_lines(s: &[u8], head: usize) -> usize {
    let (stuck_bytes, stuck) = base64_run(s, usize::MAX);
    if stuck > MAX_PEM_LINE_FULL + 1 {
        return stuck_bytes;
    }
    let mut at = head;
    let mut full = false;
    while at < head + MAX_KEY_BODY {
        let (mut gap, _) = pem_gap(&s[at..]);
        if gap == 0 && !(at == 0 && stuck > 0) {
            break;
        }
        // One blank line is allowed between the header and the body.
        let plus = diff_plus(&s[at + gap..]);
        if at == head && line_break(&s[at + gap + plus..]).is_some() {
            gap += plus + pem_gap(&s[at + gap + plus..]).0;
        }
        let start = at + gap;
        let (bytes, line) = base64_run(&s[start..], MAX_PEM_LINE_FULL + 2);
        let next = start + bytes;
        // A first line longer than any line, and mixed, is a body with its
        // line breaks stripped.
        if at == head && line > MAX_PEM_LINE_FULL + 1 {
            let (all, _) = base64_run(&s[start..], usize::MAX);
            if classes(&s[start..start + all]) >= 3 {
                return start + all;
            }
        }
        // A diff marker makes a full line one longer.
        if line == 0 || line > MAX_PEM_LINE_FULL + 1 {
            break;
        }
        // A full line may also end at a space, as in a key pasted on one line.
        let (n, br) = pem_gap(&s[next..]);
        let after = &s[next..];
        let closes =
            after.first().is_some_and(|b| b"\"'`".contains(b)) || after.starts_with(b"\\\"");
        let padded = (s[next - 1] == b'='
            || (next >= 3 && s[next - 3..next].eq_ignore_ascii_case(b"%3D")))
            && n > 0;
        let ends_line =
            br || closes || next + n == s.len() || (n > 0 && line >= PEM_LINE) || (full && padded);
        // A short first line is a key's when it is long and mixed enough that
        // no word or hash is: `head -c 40 key.pem`, or a 48-column wrap.
        let first_short =
            !full && line < PEM_LINE && !(line >= MIN_PEM_RUN && classes(&s[start..next]) >= 3);
        // The gap's line prefix takes the dashes of an `END` line.
        let end_follows = br && n >= 5 && spaced(&s[next + n - 5..], KEY_END.as_bytes()).is_some();
        if !ends_line || (first_short && !end_follows) {
            break;
        }
        at = next;
        if line < PEM_LINE {
            break;
        }
        full = true;
    }
    // Armor with no body after it is prose that looks like a header.
    if at == head { 0 } else { at }
}

/// The gap before the next run of a PEM body: spaces or tabs, then perhaps
/// a line break and a line prefix (indentation, a `>` quote, a `-` diff
/// marker, or a `+` one followed by a space). Whether it held a line break.
fn pem_gap(s: &[u8]) -> (usize, bool) {
    let blank = |b: &u8| matches!(*b, b' ' | b'\t');
    let mut spaces = 0;
    while spaces < MAX_PEM_PREFIX
        && let Some(n) = body_space(&s[spaces..])
    {
        spaces += n;
    }
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

/// How many bytes of `s` are `Name: value` lines after a key's header, up to
/// the blank line that must follow them, or 0. Legacy encrypted PEM keys
/// and PGP armor carry these before the body. A line whose value holds
/// another key header is not armor: that header reads the lines after it
/// itself, so no line is read for more than two headers.
fn armor(s: &[u8]) -> usize {
    let mut at = 0;
    loop {
        let (gap, br) = pem_gap(&s[at..]);
        let line = &s[at + gap..];
        // An added diff line's `+` is stuck to the name.
        let plus = diff_plus(line);
        let name = line[plus..]
            .iter()
            .take(MAX_ARMOR_LINE)
            .take_while(|b| b.is_ascii_alphanumeric() || **b == b'-')
            .count();
        let name = plus + name;
        if !br || name == plus || !line[name..].starts_with(b": ") {
            break;
        }
        // A run of backslashes that is not a break is skipped whole, so no
        // line reads a long run twice.
        let mut v = name;
        let mut nested = false;
        while v < name + MAX_ARMOR_LINE && v < line.len() && line_break(&line[v..]).is_none() {
            nested |= line[v] == b'-' && starts_word(line, v) && key_header(&line[v..]).is_some();
            v += match line[v] {
                b'\\' => line[v..].iter().take_while(|b| **b == b'\\').count(),
                _ => 1,
            };
        }
        if nested || line_break(&line[v.min(line.len())..]).is_none() {
            break;
        }
        at += gap + v;
    }
    let (gap, br) = pem_gap(&s[at..]);
    let blank = at + gap + diff_plus(&s[at + gap..]);
    if at > 0 && br && line_break(&s[blank..]).is_some() {
        at
    } else {
        0
    }
}

/// 1 when `s` starts with a `+` that is not base64 of the line after it: a
/// diff marker before a name or a blank line.
fn diff_plus(s: &[u8]) -> usize {
    usize::from(
        s.first() == Some(&b'+')
            && s.get(1)
                .is_some_and(|b| b.is_ascii_alphabetic() || line_break(&s[1..]).is_some()),
    )
}

/// `%2B`, `%2F` or `%3D` at the start of `s`: a base64 byte URL-encoded.
fn encoded_base64(s: &[u8]) -> bool {
    s.len() >= 3
        && s[0] == b'%'
        && matches!(
            (s[1], s[2].to_ascii_uppercase()),
            (b'2', b'B' | b'F') | (b'3', b'D')
        )
}

/// A space or tab between the runs of a key body, or `%20` in a URL-encoded
/// one.
fn body_space(s: &[u8]) -> Option<usize> {
    match s.first()? {
        b' ' | b'\t' => Some(1),
        b'%' if s.get(1..3) == Some(b"20") => Some(3),
        _ => None,
    }
}

/// A base64 byte JSON escaped, at any depth: `/` as `\/` or `\\\/`, and
/// `+`, `/` or `=` as `\u002b`, `\u002f` or `\u003d`. An even run of
/// backslashes before `/` is backslashes, so that `/` is not escaped.
fn escaped_base64(s: &[u8]) -> Option<usize> {
    let bs = s.iter().take_while(|b| **b == b'\\').count();
    if bs == 0 {
        return None;
    }
    if bs % 2 == 1 && s.get(bs) == Some(&b'/') {
        return Some(bs + 1);
    }
    let code = s.get(bs..bs + 5)?;
    [b"u002b", b"u002f", b"u003d"]
        .iter()
        .any(|c| code.eq_ignore_ascii_case(*c))
        .then_some(bs + 5)
}

/// The bytes and the base64 characters of the run at the start of `s`, at
/// most `max` characters, reading `%2B`, `%2F`, `%3D` and a JSON-escaped
/// base64 byte as one each.
fn base64_run(s: &[u8], max: usize) -> (usize, usize) {
    let (mut at, mut n) = (0, 0);
    while n < max {
        if s.get(at).is_some_and(|b| base64(*b)) {
            at += 1;
        } else if encoded_base64(&s[at..]) {
            at += 3;
        } else if let Some(k) = escaped_base64(&s[at..]) {
            at += k;
        } else {
            break;
        }
        n += 1;
    }
    (at, n)
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

/// A line break as text carries it: a real one, `\n`, `\r\n`, `\u000a` or
/// `\u000d\u000a` with any depth of escaping (JSON inside JSON), backslash
/// then a real newline, or URL-encoded as `%0A` or `%0D%0A`.
fn escaped_break(s: &[u8]) -> Option<usize> {
    if s.starts_with(b"\r\n") {
        return Some(2);
    }
    if s.starts_with(b"\n") {
        return Some(1);
    }
    let lf = |s: &[u8], code: &[u8]| {
        s.get(..code.len())
            .is_some_and(|h| h.eq_ignore_ascii_case(code))
    };
    if lf(s, b"%0A") {
        return Some(3);
    }
    if lf(s, b"%0D%0A") {
        return Some(6);
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
    if lf(rest, b"u000a") {
        return Some(bs + 5);
    }
    if lf(rest, b"u000d") {
        let more = slashes(&rest[5..]);
        if more > 0 && lf(&rest[5 + more..], b"u000a") {
            return Some(bs + 5 + more + 5);
        }
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
        WorkRecord::Context { text, .. }
        | WorkRecord::HumanInput { text, .. }
        | WorkRecord::Steered { text, .. } => {
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
        | WorkRecord::Retried { .. }
        | WorkRecord::Snoozed { .. }
        | WorkRecord::Parked { .. }
        | WorkRecord::Unblocked { .. }
        | WorkRecord::ReviewRequested { .. }
        | WorkRecord::ChangesRequested { .. }
        | WorkRecord::MergeMarked { .. }
        | WorkRecord::SummaryRated { .. }
        | WorkRecord::Taken { .. }
        | WorkRecord::Reassigned { .. }
        | WorkRecord::Reopened { .. }
        | WorkRecord::AttemptClosed { .. }
        | WorkRecord::TierChanged { .. }
        | WorkRecord::BudgetSet { .. }
        | WorkRecord::PriceSet { .. }
        | WorkRecord::SourceStalled { .. }
        | WorkRecord::FormatRaised { .. }
        | WorkRecord::Refused { .. }
        | WorkRecord::Unknown => {}
    }
}

#[cfg(test)]
mod tests;
