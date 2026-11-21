//! The one retry policy of the HTTP wire: a 429 waits the `Retry-After` (seconds or an HTTP date, never
//! past [`HOLD`]) or backs off, a failure that passes is tried again, and every other answer is the answer.

use std::time::{Duration, SystemTime};

/// How many times a request is made, the wait before the second try (doubling for each after it), its
/// ceiling, and the longest wait the server may name: one told to wait longer is answered as it was.
pub(crate) const TRIES: u32 = 4;
const FIRST: Duration = Duration::from_secs(1);
const LONGEST: Duration = Duration::from_secs(20);
pub(crate) const HOLD: Duration = Duration::from_secs(60);

/// The wait after try number `attempt` (from 1): doubling from [`FIRST`] to [`LONGEST`], its lower half
/// fixed and its upper half random, so callers that failed together do not return together.
pub(crate) fn backoff(attempt: u32) -> Duration {
    let ceiling = FIRST.saturating_mul(1 << (attempt - 1).min(8)).min(LONGEST);
    ceiling / 2 + ceiling.mul_f64(rand::random::<f64>() / 2.0)
}

/// The wait a `Retry-After` header names, in seconds or as an HTTP date; `None` for one that is malformed,
/// already past, or zero.
pub(crate) fn wait_of(value: &str) -> Option<Duration> {
    let value = value.trim();
    let wait = match value.parse::<u64>() {
        Ok(seconds) => Duration::from_secs(seconds),
        Err(_) => httpdate::parse_http_date(value).ok()?.duration_since(SystemTime::now()).unwrap_or_default(),
    };
    (!wait.is_zero()).then_some(wait)
}

/// A failure of the connection that a second try may not have: a timeout, a refusal, a reset, a body cut off.
pub(crate) fn passes(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_request() || error.is_body()
}

/// A status worth making the request again for. A 429 was never processed, so it is always made again; the
/// others may have been, so they are made again only when `repeats` says repeating the request is safe.
pub(crate) fn status(code: u16, repeats: bool) -> bool {
    code == 429 || (repeats && matches!(code, 500 | 502 | 503 | 504))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retry_after_is_seconds_or_an_http_date() {
        assert_eq!(wait_of("7"), Some(Duration::from_secs(7)));
        assert_eq!(wait_of(" 10 "), Some(Duration::from_secs(10)));
        assert_eq!(wait_of("0"), None);
        assert_eq!(wait_of("soon"), None);
        assert_eq!(wait_of("Sun, 06 Nov 1994 08:49:37 GMT"), None, "a date in the past names no wait");
        let soon = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(30));
        let wait = wait_of(&soon).unwrap();
        assert!(wait > Duration::from_secs(25) && wait <= Duration::from_secs(30), "{wait:?}");
    }

    #[test]
    fn the_backoff_doubles_to_its_ceiling_with_the_lower_half_fixed() {
        for attempt in 1..=10u32 {
            let ceiling = FIRST.saturating_mul(1 << (attempt - 1).min(8)).min(LONGEST);
            for _ in 0..50 {
                let wait = backoff(attempt);
                assert!(wait >= ceiling / 2 && wait <= ceiling, "try {attempt}: {wait:?} outside {ceiling:?}");
            }
        }
    }

    #[test]
    fn only_a_429_is_made_again_for_a_request_that_may_have_been_processed() {
        assert!(status(429, false));
        assert!(!status(503, false));
        for code in [500, 502, 503, 504] {
            assert!(status(code, true));
        }
        for code in [200, 301, 400, 401, 403, 404, 409, 422, 501] {
            assert!(!status(code, true), "{code}");
        }
    }
}
