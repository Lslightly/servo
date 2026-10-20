/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Serving range requests out of a stored response.
//! <https://httpwg.org/specs/rfc9110.html#range.requests>

use std::ops::Bound;

use headers::{ContentRange, HeaderMapExt, Range};
use http::{HeaderMap, StatusCode, header};
use net_traits::request::Request;

use crate::http_cache::store::{BodyRange, EntryMeta};

/// What part of a stored entry answers a range request.
pub(crate) struct RangeHit {
    /// Where the answer sits inside the stored body.
    pub body: BodyRange,
    /// First byte of the resource being served.
    pub first: u64,
    /// Last byte of the resource being served.
    pub last: u64,
    /// Complete length of the resource.
    pub total: u64,
}

pub(crate) enum RangeOutcome {
    /// The request did not ask for a range, and this entry holds the whole thing.
    Whole,
    /// The entry can satisfy the requested range.
    Satisfiable(RangeHit),
    /// This entry cannot answer this request. The caller goes to the network.
    Unsatisfiable,
}

/// What a stored 206 covers.
struct StoredRange {
    first: u64,
    last: u64,
    total: u64,
}

fn stored_range(headers: &HeaderMap) -> Option<StoredRange> {
    let content_range = headers.typed_get::<ContentRange>()?;
    let (first, last) = content_range.bytes_range()?;
    Some(StoredRange {
        first,
        last,
        total: content_range.bytes_len()?,
    })
}

/// Decide what part of a stored entry answers `request`.
///
/// Ranges refer to offsets in the decoded body, but entries are stored exactly as
/// received, so an entry with a content coding cannot serve one. This is also what
/// Chromium does.
pub(crate) fn select(request: &Request, meta: &EntryMeta) -> RangeOutcome {
    let partial = meta.status.try_code() == Some(StatusCode::PARTIAL_CONTENT);
    let stored = stored_range(&meta.headers);
    if partial && stored.is_none() {
        // A 206 whose `Content-Range` cannot be read says nothing about what it holds.
        return RangeOutcome::Unsatisfiable;
    }

    let spec = request
        .headers
        .typed_get::<Range>()
        // A `Range` whose `If-Range` condition fails is ignored, and the whole
        // resource is asked for instead.
        // <https://httpwg.org/specs/rfc9110.html#field.if-range>
        .filter(|_| if_range_matches(request, &meta.headers));
    let Some(spec) = spec else {
        // A partial response can never answer a request for the whole resource.
        return if partial {
            RangeOutcome::Unsatisfiable
        } else {
            RangeOutcome::Whole
        };
    };
    if meta.content_encoding.is_some() || meta.body_len == 0 {
        return RangeOutcome::Unsatisfiable;
    }

    // Only the bytes really stored can be served, however much a `Content-Range`
    // claims. A partial that holds fewer bytes than it claims *and* claims to reach
    // the end of the resource is a server describing a shorter resource than it
    // said, so the resource is taken to end where the stored bytes do: that shorter
    // length is both what a suffix range resolves against and what the served
    // `Content-Range` reports, so the two cannot disagree. `partial.any.js` stores
    // exactly such an entry.
    let (first, covered_last, total) = match stored {
        Some(stored) => {
            let last_held = stored
                .first
                .checked_add(meta.body_len)
                .and_then(|end| end.checked_sub(1));
            let Some(covered_last) = last_held.map(|held| held.min(stored.last)) else {
                return RangeOutcome::Unsatisfiable;
            };
            let total = if stored.last.checked_add(1) == Some(stored.total) {
                covered_last + 1
            } else {
                stored.total
            };
            (stored.first, covered_last, total)
        },
        // A complete response covers the whole resource.
        None => (0, meta.body_len - 1, meta.body_len),
    };
    if total == 0 || covered_last < first {
        return RangeOutcome::Unsatisfiable;
    }

    let mut ranges = spec.satisfiable_ranges(total);
    let Some(bounds) = ranges.next() else {
        return RangeOutcome::Unsatisfiable;
    };
    if ranges.next().is_some() {
        // A multipart answer is not something this cache builds.
        return RangeOutcome::Unsatisfiable;
    }
    let (start, end) = match bounds {
        (Bound::Included(start), Bound::Included(end)) => (start, end.min(total - 1)),
        (Bound::Included(start), Bound::Unbounded) => (start, total - 1),
        _ => return RangeOutcome::Unsatisfiable,
    };
    if start > end || start < first || end > covered_last {
        return RangeOutcome::Unsatisfiable;
    }

    RangeOutcome::Satisfiable(RangeHit {
        body: BodyRange {
            start: start - first,
            end: end - first,
        },
        first: start,
        last: end,
        total,
    })
}

/// Whether a request's `If-Range` condition holds for the stored response. A
/// request without one is unconditional.
fn if_range_matches(request: &Request, stored_headers: &HeaderMap) -> bool {
    let Some(condition) = request.headers.get(header::IF_RANGE) else {
        return true;
    };
    let condition = condition.as_bytes();
    // An entity-tag must match strongly, a date exactly.
    if condition.starts_with(b"\"") {
        return stored_headers
            .get(header::ETAG)
            .is_some_and(|etag| etag.as_bytes() == condition);
    }
    stored_headers
        .get(header::LAST_MODIFIED)
        .is_some_and(|modified| modified.as_bytes() == condition)
}

/// Rewrite a cached response's headers to describe the partial body being served.
pub(crate) fn apply_headers(headers: &mut HeaderMap, hit: &RangeHit) {
    headers.typed_insert(
        ContentRange::bytes(hit.first..=hit.last, hit.total)
            .expect("the range was checked against the stored body, so it is representable"),
    );
    headers.typed_insert(headers::ContentLength(hit.last - hit.first + 1));
}
