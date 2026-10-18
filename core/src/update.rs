//! The firmware update check: which release is the newest on GitHub, and is it newer than the one running? The clock asks the
//! GitHub API (`/repos/<owner>/<repo>/releases/latest`) over TLS, reads the first few KB of the JSON answer and takes the
//! `tag_name`. Nothing is installed; the result is shown in `status` and on the web page. (Installing is a separate step that
//! needs a signature check, see ADR-0006.)

use core::fmt::Write;

pub const HOST: &str = "api.github.com";
pub const PATH: &str = "/repos/dc0sk/picoclock/releases/latest";
/// Where the user is pointed to read about and download a release.
pub const RELEASES_URL: &str = "https://github.com/dc0sk/picoclock/releases";

/// A release number `major.minor.patch` (a leading `v` in the tag is allowed; anything after the patch number, such as a
/// pre-release suffix, is not).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut it = s.split('.');
        let mut next = || {
            let part = it.next()?;
            // digits only: no sign, no space, no leading zeros beyond a single "0"
            if part.is_empty() || part.len() > 5 || !part.bytes().all(|b| b.is_ascii_digit()) || (part.len() > 1 && part.starts_with('0')) {
                return None;
            }
            part.parse::<u16>().ok()
        };
        let v = Version { major: next()?, minor: next()?, patch: next()? };
        it.next().is_none().then_some(v)
    }

    /// The version this firmware was built as.
    pub fn running() -> Version {
        Version::parse(crate::VERSION).expect("the package version is major.minor.patch")
    }
}

impl core::fmt::Display for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The HTTP request for the newest release. GitHub requires a `User-Agent`.
pub fn request() -> heapless::String<320> {
    let mut s = heapless::String::new();
    let _ = write!(
        s,
        "GET {PATH} HTTP/1.1\r\nHost: {HOST}\r\nUser-Agent: picoclock/{}\r\nAccept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\nConnection: close\r\n\r\n",
        crate::VERSION
    );
    s
}

/// The state of the check, as `status` shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Check {
    #[default]
    Never,
    /// A check is running (or has been asked for).
    Checking,
    Newer,
    UpToDate,
    Ahead,
    NoRelease,
    RateLimited,
    /// GitHub could not be reached, the connection or certificate failed, or the answer made no sense.
    Failed,
}

impl From<Outcome> for (Check, Option<Version>) {
    fn from(o: Outcome) -> Self {
        match o {
            Outcome::Newer(v) => (Check::Newer, Some(v)),
            Outcome::UpToDate(v) => (Check::UpToDate, Some(v)),
            Outcome::Ahead(v) => (Check::Ahead, Some(v)),
            Outcome::NoRelease => (Check::NoRelease, None),
            Outcome::RateLimited => (Check::RateLimited, None),
            Outcome::Unreadable => (Check::Failed, None),
        }
    }
}

/// The text after `update  : ` in `status` (and printed by `update`).
pub fn describe(out: &mut impl Write, enabled: bool, check: Check, latest: Option<Version>, checked_unix: Option<i64>, now_unix: Option<i64>) {
    let running = Version::running();
    let latest = latest.unwrap_or(running);
    let _ = match check {
        Check::Never if !enabled => write!(out, "checking is off (set update_check on, or type 'update check' once)"),
        Check::Never => write!(out, "not checked yet"),
        Check::Checking => write!(out, "checking GitHub..."),
        Check::Newer => write!(out, "{latest} is available, you run {running}; see {RELEASES_URL}"),
        Check::UpToDate => write!(out, "up to date ({running})"),
        Check::Ahead => write!(out, "you run {running}, newer than the latest release {latest}"),
        Check::NoRelease => write!(out, "no release has been published yet"),
        Check::RateLimited => write!(out, "GitHub refused for now (rate limit); will try again later"),
        Check::Failed => write!(out, "the last check failed (no connection, certificate or answer)"),
    };
    if let (Some(at), Some(now), true) = (checked_unix, now_unix, !matches!(check, Check::Never | Check::Checking)) {
        let ago = (now - at).max(0);
        let _ = if ago < 3600 { write!(out, ", checked {} min ago", ago / 60) } else if ago < 172_800 { write!(out, ", checked {} h ago", ago / 3600) } else { write!(out, ", checked {} days ago", ago / 86_400) };
    }
}

/// What the check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A release newer than the one running.
    Newer(Version),
    /// The newest release is the one running.
    UpToDate(Version),
    /// The running firmware is newer than the newest release (a development build).
    Ahead(Version),
    /// The repository has no release yet (GitHub answers 404).
    NoRelease,
    /// GitHub refuses for now (403 or 429, rate limit).
    RateLimited,
    /// Any other answer, or one that could not be read.
    Unreadable,
}

/// Reads the start of GitHub's answer (status line, headers, the first part of the body).
pub fn parse_response(response: &[u8], running: Version) -> Outcome {
    let Some(head_end) = response.windows(4).position(|w| w == b"\r\n\r\n") else { return Outcome::Unreadable };
    let Ok(head) = core::str::from_utf8(&response[..head_end]) else { return Outcome::Unreadable };
    let status = head.lines().next().unwrap_or("").split(' ').nth(1).unwrap_or("");
    match status {
        "200" => {}
        "404" => return Outcome::NoRelease,
        "403" | "429" => return Outcome::RateLimited,
        _ => return Outcome::Unreadable,
    }
    let body = &response[head_end + 4..];
    let Some(tag) = tag_name(body).and_then(Version::parse) else { return Outcome::Unreadable };
    match tag.cmp(&running) {
        core::cmp::Ordering::Greater => Outcome::Newer(tag),
        core::cmp::Ordering::Equal => Outcome::UpToDate(tag),
        core::cmp::Ordering::Less => Outcome::Ahead(tag),
    }
}

/// The value of the first `"tag_name": "..."` in `body`. A search of the raw bytes rather than a JSON parse, because only the
/// start of a long answer is read, and because a chunked answer has chunk sizes in it.
fn tag_name(body: &[u8]) -> Option<&str> {
    const KEY: &[u8] = b"\"tag_name\"";
    let at = body.windows(KEY.len()).position(|w| w == KEY)? + KEY.len();
    let rest = &body[at..];
    let colon = rest.iter().position(|b| !b.is_ascii_whitespace())?;
    if rest[colon] != b':' {
        return None;
    }
    let rest = &rest[colon + 1..];
    let quote = rest.iter().position(|b| !b.is_ascii_whitespace())?;
    if rest[quote] != b'"' {
        return None;
    }
    let value = &rest[quote + 1..];
    let end = value.iter().position(|&b| b == b'"')?;
    // a tag has no escapes or control characters worth supporting: refuse them rather than guess
    if value[..end].iter().any(|&b| b == b'\\' || b < 0x20) {
        return None;
    }
    core::str::from_utf8(&value[..end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: Version = Version { major: 0, minor: 2, patch: 0 };
    fn answer(status: &str, body: &str) -> std::vec::Vec<u8> {
        format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    // VERIFIES: REQ-UPD-02
    #[test]
    fn versions_parse_strictly_and_compare_by_number() {
        for (s, want) in [("v0.2.0", Some((0, 2, 0))), ("0.10.3", Some((0, 10, 3))), ("v12.0.65535", Some((12, 0, 65535))), ("1.2", None), ("1.2.3.4", None),
            ("v1.2.3-rc1", None), ("1.2.3 ", None), ("", None), ("v", None), ("1.-2.3", None), ("1.02.3", None), ("1.2.65536", None), ("a.b.c", None), ("+1.2.3", None)] {
            assert_eq!(Version::parse(s).map(|v| (v.major, v.minor, v.patch)), want, "{s:?}");
        }
        assert!(Version::parse("0.10.0") > Version::parse("0.9.9"), "numbers, not text");
        assert!(Version::parse("1.0.0") > Version::parse("0.99.99"));
        assert_eq!(Version::running().to_string(), crate::VERSION);
        assert_eq!(Version::parse("v0.2.1").unwrap().to_string(), "0.2.1");
    }

    // VERIFIES: REQ-UPD-02 REQ-UPD-01
    #[test]
    fn the_answer_is_judged_against_the_running_version() {
        let body = |tag: &str| format!("{{\"url\":\"https://api.github.com/x\",\"id\":1,\"author\":{{\"login\":\"dc0sk\"}},\"tag_name\":\"{tag}\",\"name\":\"Release\"}}");
        let v = |a, b, c| Version { major: a, minor: b, patch: c };
        assert_eq!(parse_response(&answer("200 OK", &body("v0.3.0")), RUNNING), Outcome::Newer(v(0, 3, 0)));
        assert_eq!(parse_response(&answer("200 OK", &body("v0.2.1")), RUNNING), Outcome::Newer(v(0, 2, 1)));
        assert_eq!(parse_response(&answer("200 OK", &body("0.2.0")), RUNNING), Outcome::UpToDate(v(0, 2, 0)));
        assert_eq!(parse_response(&answer("200 OK", &body("v0.1.0")), RUNNING), Outcome::Ahead(v(0, 1, 0)));
        assert_eq!(parse_response(&answer("404 Not Found", "{\"message\":\"Not Found\"}"), RUNNING), Outcome::NoRelease);
        assert_eq!(parse_response(&answer("403 Forbidden", "{\"message\":\"API rate limit exceeded\"}"), RUNNING), Outcome::RateLimited);
        assert_eq!(parse_response(&answer("429 Too Many Requests", ""), RUNNING), Outcome::RateLimited);
        // everything else is unreadable, never a guess
        for bad in [answer("500 Internal Server Error", ""), answer("200 OK", &body("v0.3.0-rc1")), answer("200 OK", "{\"name\":\"x\"}"), answer("200 OK", "{\"tag_name\":7}"),
            answer("200 OK", "{\"tag_name\":\"v0.3.0"), answer("200 OK", "{\"tag_name\":\"v0\\u002e3.0\"}"), answer("301 Moved Permanently", &body("v9.9.9")),
            b"HTTP/1.1 200 OK\r\n".to_vec(), b"garbage".to_vec(), vec![0xFF; 20], vec![]] {
            assert_eq!(parse_response(&bad, RUNNING), Outcome::Unreadable, "{:?}", String::from_utf8_lossy(&bad));
        }
        // spaces around the colon, and a chunked answer with a chunk size line in front of the JSON
        assert_eq!(parse_response(&answer("200 OK", "{ \"tag_name\" : \"v0.4.0\" }"), RUNNING), Outcome::Newer(v(0, 4, 0)));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3a\r\n{\"tag_name\":\"v0.5.0\"}\r\n0\r\n\r\n".to_vec();
        assert_eq!(parse_response(&chunked, RUNNING), Outcome::Newer(v(0, 5, 0)));
    }

    // VERIFIES: REQ-UPD-01
    #[test]
    fn status_says_what_was_found_and_never_that_anything_was_installed() {
        let line = |enabled, check, latest, at: Option<i64>| {
            let mut s = std::string::String::new();
            describe(&mut s, enabled, check, latest, at, Some(1_000_000));
            s
        };
        let v = Version::parse("0.3.0");
        assert!(line(false, Check::Never, None, None).contains("checking is off"));
        assert_eq!(line(true, Check::Never, None, None), "not checked yet");
        assert!(line(true, Check::Checking, None, Some(0)).starts_with("checking GitHub"));
        let newer = line(true, Check::Newer, v, Some(1_000_000 - 7200));
        assert!(newer.starts_with(&format!("0.3.0 is available, you run {}", crate::VERSION)) && newer.contains(RELEASES_URL) && newer.ends_with("checked 2 h ago"), "{newer}");
        assert!(line(true, Check::UpToDate, v, Some(1_000_000 - 90)).ends_with(", checked 1 min ago"));
        assert!(line(true, Check::NoRelease, None, Some(1_000_000 - 3 * 86_400)).ends_with("checked 3 days ago"));
        for c in [Check::Ahead, Check::RateLimited, Check::Failed] {
            assert!(line(true, c, v, None).len() > 20 && !line(true, c, v, None).contains("install"), "{c:?}");
        }
        // every outcome maps to a state; only a readable answer carries a version
        assert_eq!(<(Check, Option<Version>)>::from(Outcome::Newer(v.unwrap())), (Check::Newer, v));
        assert_eq!(<(Check, Option<Version>)>::from(Outcome::Unreadable), (Check::Failed, None));
        assert_eq!(<(Check, Option<Version>)>::from(Outcome::NoRelease), (Check::NoRelease, None));
    }

    // VERIFIES: REQ-UPD-01
    #[test]
    fn the_request_names_the_release_endpoint_and_the_running_version() {
        let r = request();
        assert!(r.starts_with("GET /repos/dc0sk/picoclock/releases/latest HTTP/1.1\r\nHost: api.github.com\r\n"));
        assert!(r.contains(&format!("User-Agent: picoclock/{}\r\n", crate::VERSION)) && r.ends_with("Connection: close\r\n\r\n"));
        assert!(RELEASES_URL.starts_with("https://github.com/dc0sk/picoclock"));
    }
}
