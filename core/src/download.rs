//! Fetching the update file of the newest release from GitHub (ADR-0006, step 2, Pico 2 W only). The clock asks
//! `github.com` for the release asset, gets a redirect to GitHub's download host, and reads the file from there. This module
//! holds what can be decided without a network: the two requests, which redirect target is acceptable, and a reader for the
//! response head that keeps only the few headers that matter (GitHub's redirect carries a content-security-policy of several
//! KB, which must not have to fit anywhere).
//!
//! Nothing here trusts the download: the file is an ordinary update file (`image`) and goes through the same checks as an
//! upload, signature first.

use core::fmt::Write;

use heapless::String;

use crate::update::Version;

/// Where the release asset is asked for. The certificate chain ends at the same root as the update check's.
pub const HOST: &str = "github.com";
/// The only host a redirect may lead to: GitHub's release asset storage. Its chain ends at ISRG Root X1 (RSA), which only the
/// Pico 2 W can verify; a redirect anywhere else is refused.
pub const ASSET_HOST: &str = "release-assets.githubusercontent.com";
/// Longest path and query accepted from a redirect. GitHub's carries a signed token and is about 1.1 KB today.
pub const MAX_LOCATION: usize = 2048;

/// The file name of the update file of a release for the Pico 2 W, which the release script uploads.
pub fn asset_name(v: Version) -> String<48> {
    let mut s = String::new();
    let _ = write!(s, "picoclock-{v}-pico2-w.pcu");
    s
}

fn request_for(host: &str, path: &str) -> String<{ MAX_LOCATION + 256 }> {
    let mut s = String::new();
    let _ = write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: picoclock/{}\r\nAccept: application/octet-stream\r\nConnection: close\r\n\r\n", crate::VERSION);
    s
}

/// The first request: the release asset on github.com, which answers with a redirect.
pub fn first_request(v: Version) -> String<{ MAX_LOCATION + 256 }> {
    let mut path = String::<160>::new();
    let _ = write!(path, "/dc0sk/picoclock/releases/download/v{v}/{}", asset_name(v));
    request_for(HOST, &path)
}

/// The second request, to the host the redirect named. `location` is the `Location` header's value; it must be an `https`
/// URL on [`ASSET_HOST`] (no port, no user info) and contain only visible ASCII, so that nothing in it can end the request line
/// or add a header. Returns `None` for anything else.
pub fn second_request(location: &str) -> Option<String<{ MAX_LOCATION + 256 }>> {
    let rest = location.strip_prefix("https://")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    if host != ASSET_HOST || path.len() > MAX_LOCATION || !path.bytes().all(|b| (0x21..=0x7E).contains(&b)) {
        return None;
    }
    Some(request_for(host, path))
}

/// What the response head said, as far as it matters here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Head {
    pub status: u16,
    pub content_length: Option<usize>,
    pub location: Option<String<MAX_LOCATION>>,
    /// `Transfer-Encoding` was present (this client does not decode chunks and refuses the answer).
    pub chunked: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HeadError {
    /// Not an HTTP/1.x status line, or a header line without a colon, or a status or length that is no number.
    Garbage,
    /// The head is longer than the reader accepts, or a `Location` does not fit.
    TooLong,
}

/// Longest header line looked at; longer ones (such as GitHub's content-security-policy) are skipped unseen.
const LINE: usize = MAX_LOCATION + 16;
/// Longest head accepted altogether.
const MAX_HEAD: usize = 24 * 1024;

/// Reads a response head piece by piece (as the TLS records arrive) and remembers status, `Content-Length`, `Location`.
pub struct HeadScanner {
    line: String<LINE>,
    /// The current line is longer than `LINE`: skipped to its end.
    skipping: bool,
    first: bool,
    seen: usize,
    head: Head,
}

impl Default for HeadScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl HeadScanner {
    pub fn new() -> Self {
        Self { line: String::new(), skipping: false, first: true, seen: 0, head: Head::default() }
    }

    /// Feeds the next bytes. `Ok(Some((n, head)))`: the head ended `n` bytes into `data` (the body starts there); `Ok(None)`:
    /// more is needed.
    pub fn feed(&mut self, data: &[u8]) -> Result<Option<(usize, Head)>, HeadError> {
        for (i, &b) in data.iter().enumerate() {
            self.seen += 1;
            if self.seen > MAX_HEAD {
                return Err(HeadError::TooLong);
            }
            if b != b'\n' {
                if !self.skipping {
                    // bytes outside ASCII cannot be part of anything this reader uses; a non-UTF-8 line is skipped as long
                    if b >= 0x80 || self.line.push(b as char).is_err() {
                        self.long_line()?;
                    }
                }
                continue;
            }
            let line = core::mem::take(&mut self.line);
            let skipped = core::mem::take(&mut self.skipping);
            let line = line.strip_suffix('\r').unwrap_or(&line);
            if self.first {
                if skipped {
                    return Err(HeadError::Garbage);
                }
                self.first = false;
                let mut parts = line.splitn(3, ' ');
                let (Some(version), Some(code)) = (parts.next(), parts.next()) else { return Err(HeadError::Garbage) };
                if !version.starts_with("HTTP/1.") {
                    return Err(HeadError::Garbage);
                }
                self.head.status = code.parse().map_err(|_| HeadError::Garbage)?;
            } else if line.is_empty() && !skipped {
                return Ok(Some((i + 1, core::mem::take(&mut self.head))));
            } else if !skipped {
                self.header(line)?;
            }
        }
        Ok(None)
    }

    /// The line does not fit. Only a `Location` must not be lost this way; any other header is skipped.
    fn long_line(&mut self) -> Result<(), HeadError> {
        if self.line.len() >= 9 && self.line.as_bytes()[..9].eq_ignore_ascii_case(b"location:") {
            return Err(HeadError::TooLong);
        }
        if self.first {
            return Err(HeadError::Garbage);
        }
        self.line.clear();
        self.skipping = true;
        Ok(())
    }

    fn header(&mut self, line: &str) -> Result<(), HeadError> {
        let Some((name, value)) = line.split_once(':') else { return Err(HeadError::Garbage) };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            // a second, different length is a smuggling signal, not something to pick from
            let n: usize = value.parse().map_err(|_| HeadError::Garbage)?;
            if self.head.content_length.is_some_and(|old| old != n) {
                return Err(HeadError::Garbage);
            }
            self.head.content_length = Some(n);
        } else if name.eq_ignore_ascii_case("location") {
            self.head.location = Some(String::try_from(value).map_err(|_| HeadError::TooLong)?);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            self.head.chunked = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: Version = Version { major: 0, minor: 3, patch: 0 };

    fn scan(parts: &[&[u8]]) -> Result<Option<(usize, Head)>, HeadError> {
        let mut s = HeadScanner::new();
        let mut result = Ok(None);
        for p in parts {
            result = s.feed(p);
            if !matches!(result, Ok(None)) {
                break;
            }
        }
        result
    }

    // VERIFIES: REQ-UPD-09
    #[test]
    fn the_requests_name_the_release_asset_and_only_the_download_host_is_followed() {
        assert_eq!(asset_name(V).as_str(), "picoclock-0.3.0-pico2-w.pcu");
        let first = first_request(V);
        assert!(first.starts_with("GET /dc0sk/picoclock/releases/download/v0.3.0/picoclock-0.3.0-pico2-w.pcu HTTP/1.1\r\nHost: github.com\r\n"), "{first}");
        assert!(first.ends_with("Connection: close\r\n\r\n"));
        let ok = format!("https://{ASSET_HOST}/github-production-release-asset/1/abc?sp=r&sig=a%2Bb&jwt=e.y.z");
        let second = second_request(&ok).expect("the download host is followed");
        assert!(second.starts_with("GET /github-production-release-asset/1/abc?sp=r&sig=a%2Bb&jwt=e.y.z HTTP/1.1\r\nHost: release-assets.githubusercontent.com\r\n"), "{second}");
        for bad in [
            format!("http://{ASSET_HOST}/x"),
            "https://github.com/x".into(),
            format!("https://{ASSET_HOST}.evil.example/x"),
            format!("https://evil.example/{ASSET_HOST}/x"),
            format!("https://user@{ASSET_HOST}/x"),
            format!("https://{ASSET_HOST}:8443/x"),
            format!("https://{ASSET_HOST}"),
            format!("https://{ASSET_HOST}/x y"),
            format!("https://{ASSET_HOST}/x\r\nHost: evil.example"),
            format!("https://{ASSET_HOST}/{}", "a".repeat(MAX_LOCATION)),
            "/relative/path".into(),
            std::string::String::new(),
        ] as [std::string::String; 12]
        {
            assert!(second_request(&bad).is_none(), "{bad:?}");
        }
    }

    // VERIFIES: REQ-UPD-09
    #[test]
    fn the_head_reader_keeps_status_length_and_location_whatever_else_comes() {
        let huge_csp = format!("content-security-policy: default-src 'none'; {}\r\n", "x".repeat(5000));
        let head = "HTTP/1.1 302 Found\r\nX-Other: 1\r\n";
        let text = format!("{head}Location: https://{ASSET_HOST}/a?b=c\r\n{huge_csp}Content-Length: 0\r\nServer: github.com\r\n\r\nBODY");
        let (n, h) = scan(&[text.as_bytes()]).unwrap().expect("complete");
        assert_eq!(&text.as_bytes()[n..], b"BODY");
        assert_eq!((h.status, h.content_length, h.chunked), (302, Some(0), false));
        assert_eq!(h.location.as_deref(), Some(format!("https://{ASSET_HOST}/a?b=c").as_str()));
        // the same head in pieces of every size, including one byte at a time, gives the same answer
        for size in [1, 2, 3, 7, 64, 1000] {
            let mut s = HeadScanner::new();
            let mut out = None;
            for piece in text.as_bytes().chunks(size) {
                if let Some(done) = s.feed(piece).unwrap() {
                    out = Some(done);
                    break;
                }
            }
            let (_, h2) = out.expect("complete");
            assert_eq!(h2, h, "pieces of {size}");
        }
        // an answer with a body: the length, no location
        let (_, h) = scan(&[b"HTTP/1.1 200 OK\r\ncontent-length: 1388272\r\nContent-Type: application/octet-stream\r\n\r\n"]).unwrap().unwrap();
        assert_eq!((h.status, h.content_length, h.location), (200, Some(1_388_272), None));
    }

    // VERIFIES: REQ-UPD-09
    #[test]
    fn a_head_that_is_odd_is_refused_rather_than_guessed() {
        let long_location = format!("HTTP/1.1 302 Found\r\nLocation: https://{ASSET_HOST}/{}\r\n\r\n", "a".repeat(MAX_LOCATION + 100));
        for (bad, why) in [
            (&b"garbage\r\n\r\n"[..], HeadError::Garbage),
            (b"HTTP/1.1\r\n\r\n", HeadError::Garbage),
            (b"HTTP/1.1 abc OK\r\n\r\n", HeadError::Garbage),
            (b"SMTP/1.1 200 OK\r\n\r\n", HeadError::Garbage),
            (b"HTTP/1.1 200 OK\r\nno colon here\r\n\r\n", HeadError::Garbage),
            (b"HTTP/1.1 200 OK\r\nContent-Length: ten\r\n\r\n", HeadError::Garbage),
            (b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\n", HeadError::Garbage),
            (long_location.as_bytes(), HeadError::TooLong),
        ] {
            assert_eq!(scan(&[bad]).unwrap_err(), why, "{:?}", std::string::String::from_utf8_lossy(bad));
        }
        // the same length twice is harmless; chunked is noted so the caller can refuse it
        let (_, h) = scan(&[b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\ncontent-length: 5\r\nTransfer-Encoding: chunked\r\n\r\n"]).unwrap().unwrap();
        assert_eq!((h.content_length, h.chunked), (Some(5), true));
        // endless header bytes are cut off
        let mut s = HeadScanner::new();
        s.feed(b"HTTP/1.1 200 OK\r\n").unwrap();
        let filler = [b'a'; 1000];
        let mut result = Ok(None);
        for _ in 0..40 {
            result = s.feed(&filler);
            if result.is_err() {
                break;
            }
        }
        assert_eq!(result, Err(HeadError::TooLong));
        // incomplete is not an error
        assert_eq!(scan(&[b"HTTP/1.1 200 OK\r\nContent-Le"]).unwrap(), None);
    }
}
