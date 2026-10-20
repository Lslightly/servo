/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::Arc;

use http::header::{
    CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_LOCATION, CONTENT_RANGE, DATE, ETAG,
    EXPIRES, HeaderName, HeaderValue, LOCATION, RANGE, SET_COOKIE, VARY,
};
use http::{HeaderMap, Method, StatusCode};
use net::http_cache::memory_store::MemoryStore;
use net::http_cache::{HttpCache, ValidationStatus};
use net_traits::blob_url_store::UrlWithBlobClaim;
use net_traits::request::{
    Destination, InsecureRequestsPolicy, Origin, Referrer, Request, RequestBuilder, RequestClient,
};
use net_traits::response::Response;
use net_traits::{ResourceFetchTiming, ResourceTimingType};
use servo_base::id::TEST_PIPELINE_ID;
use servo_url::ServoUrl;

const URL: &str = "https://servo.org/";

fn test_cache() -> HttpCache {
    HttpCache::with_store(Arc::new(MemoryStore::new(16 * 1024 * 1024)))
}

fn request_with_headers(headers: HeaderMap) -> Request {
    let url = ServoUrl::parse(URL).unwrap();
    RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url.clone(), None),
        Referrer::NoReferrer,
    )
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .origin(url.origin())
    .headers(headers)
    .build()
}

fn request() -> Request {
    request_with_headers(HeaderMap::new())
}

fn request_for(url: &str, headers: &[(HeaderName, &str)]) -> Request {
    let url = ServoUrl::parse(url).unwrap();
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        map.insert(name.clone(), HeaderValue::from_str(value).unwrap());
    }
    RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url.clone(), None),
        Referrer::NoReferrer,
    )
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .origin(url.origin())
    .headers(map)
    .build()
}

/// A request made by a document, which can revalidate in the background.
fn request_with_client() -> Request {
    let mut request = request();
    request.client = Some(RequestClient {
        preloaded_resources: Default::default(),
        policy_container: Default::default(),
        origin: Origin::Client,
        is_nested_browsing_context: false,
        insecure_requests_policy: InsecureRequestsPolicy::DoNotUpgrade,
        has_trustworthy_ancestor_origin: false,
        network_partition_key: None,
    });
    request
}

fn response_with_headers(headers: &[(http::HeaderName, &str)]) -> Response {
    let url = ServoUrl::parse(URL).unwrap();
    let timing = ResourceFetchTiming::new(ResourceTimingType::Navigation);
    let mut response = Response::new(url, timing);
    response.status = StatusCode::OK.into();
    for (name, value) in headers {
        response
            .headers
            .insert(name.clone(), HeaderValue::from_str(value).unwrap());
    }
    response
}

#[tokio::test]
async fn test_fresh_response_is_valid() {
    let cache = test_cache();
    let request = request();
    let response = response_with_headers(&[(CACHE_CONTROL, "max-age=1000")]);
    assert!(cache.store_for_test(&request, &response, b"body").await);

    assert_eq!(cache.probe(&request).await, Some(ValidationStatus::Valid));
    assert_eq!(
        cache.read_body_for_test(&request).await.as_deref(),
        Some(&b"body"[..])
    );
}

#[tokio::test]
async fn test_url_credentials_and_fragment_are_not_part_of_the_key() {
    let cache = test_cache();
    let response = response_with_headers(&[(CACHE_CONTROL, "max-age=3600")]);
    assert!(
        cache
            .store_for_test(
                &request_for("https://user:secret@servo.org/private#top", &[]),
                &response,
                b"body",
            )
            .await
    );
    // The key is written into the entry, and must not carry the password.
    assert_eq!(
        cache
            .read_body_for_test(&request_for("https://servo.org/private", &[]))
            .await
            .as_deref(),
        Some(&b"body"[..])
    );
}

#[tokio::test]
async fn test_no_store_response_is_not_stored() {
    let cache = test_cache();
    let request = request();
    let response = response_with_headers(&[(CACHE_CONTROL, "no-store")]);
    assert!(!cache.store_for_test(&request, &response, b"body").await);
    assert_eq!(cache.probe(&request).await, None);
}

fn partial_response() -> Response {
    let mut response = response_with_headers(&[
        (CACHE_CONTROL, "max-age=1000"),
        (CONTENT_RANGE, "bytes 0-4/10"),
        (CONTENT_LENGTH, "5"),
    ]);
    response.status = StatusCode::PARTIAL_CONTENT.into();
    response
}

#[tokio::test]
async fn test_partial_content_never_answers_a_request_for_the_whole_resource() {
    let cache = test_cache();
    let request = request();
    assert!(
        cache
            .store_for_test(&request, &partial_response(), b"12345")
            .await
    );
    assert_eq!(
        cache.probe(&request).await,
        None,
        "a stored 206 must not answer a request that asks for the whole resource"
    );
}

#[tokio::test]
async fn test_partial_content_serves_a_range_it_covers() {
    let cache = test_cache();
    assert!(
        cache
            .store_for_test(&request(), &partial_response(), b"12345")
            .await
    );
    let mut headers = HeaderMap::new();
    headers.insert(RANGE, HeaderValue::from_static("bytes=1-3"));
    assert_eq!(
        cache.probe(&request_with_headers(headers)).await,
        Some(ValidationStatus::Valid)
    );
}

#[tokio::test]
async fn test_skip_incomplete_cache_for_range_request_with_no_end_bound() {
    // The stored 206 holds bytes 0 to 4 of a 10-byte resource, so it cannot answer
    // a request for everything from byte 0 onwards.
    let cache = test_cache();
    assert!(
        cache
            .store_for_test(&request(), &partial_response(), b"12345")
            .await
    );

    let mut headers = HeaderMap::new();
    headers.insert(RANGE, HeaderValue::from_static("bytes=0-"));
    assert_eq!(cache.probe(&request_with_headers(headers)).await, None);
}

#[tokio::test]
async fn test_partial_content_does_not_serve_a_range_beyond_what_it_holds() {
    // The `Content-Range` claims five bytes, the body is longer: only what the
    // claim covers may be served, whatever the stored body's length says.
    let cache = test_cache();
    assert!(
        cache
            .store_for_test(&request(), &partial_response(), b"12345678901234567890")
            .await
    );

    let mut headers = HeaderMap::new();
    headers.insert(RANGE, HeaderValue::from_static("bytes=0-9"));
    assert_eq!(cache.probe(&request_with_headers(headers)).await, None);
}

#[tokio::test]
async fn test_partial_content_does_not_answer_a_suffix_range_it_does_not_hold() {
    // A partial that claims to reach the end of the resource but holds less of it
    // must not answer a suffix range with the wrong bytes.
    let cache = test_cache();
    let mut response = response_with_headers(&[
        (CACHE_CONTROL, "max-age=1000"),
        (CONTENT_RANGE, "bytes 0-999/1000"),
    ]);
    response.status = StatusCode::PARTIAL_CONTENT.into();
    assert!(cache.store_for_test(&request(), &response, b"12345").await);

    let mut headers = HeaderMap::new();
    headers.insert(RANGE, HeaderValue::from_static("bytes=-100"));
    assert_eq!(
        cache.probe(&request_with_headers(headers)).await,
        None,
        "the last 100 bytes of the resource are not among the 5 that are stored"
    );
}

#[tokio::test]
async fn test_range_is_ignored_when_if_range_does_not_match() {
    let cache = test_cache();
    let response = response_with_headers(&[(CACHE_CONTROL, "max-age=1000"), (ETAG, "\"a\"")]);
    assert!(cache.store_for_test(&request(), &response, b"body").await);

    let mut headers = HeaderMap::new();
    headers.insert(RANGE, HeaderValue::from_static("bytes=0-1"));
    headers.insert("if-range", HeaderValue::from_static("\"b\""));
    // The condition fails, so the request is for the whole resource, which this
    // entry does hold.
    assert_eq!(
        cache.probe(&request_with_headers(headers)).await,
        Some(ValidationStatus::Valid)
    );
}

#[tokio::test]
async fn test_multiple_ranges_are_not_served_from_the_cache() {
    let cache = test_cache();
    let response = response_with_headers(&[(CACHE_CONTROL, "max-age=1000")]);
    assert!(
        cache
            .store_for_test(&request(), &response, b"0123456789")
            .await
    );

    let mut headers = HeaderMap::new();
    headers.insert(RANGE, HeaderValue::from_static("bytes=0-1,4-5"));
    assert_eq!(
        cache.probe(&request_with_headers(headers)).await,
        None,
        "a multipart answer is not something the cache builds"
    );
}

#[tokio::test]
async fn test_vary_star_is_not_stored() {
    let cache = test_cache();
    let response = response_with_headers(&[(CACHE_CONTROL, "max-age=1000"), (VARY, "*")]);
    assert!(
        !cache.store_for_test(&request(), &response, b"body").await,
        "a `Vary: *` entry can never match a later request"
    );
}

#[tokio::test]
async fn test_set_cookie_is_not_kept_by_the_cache() {
    let cache = test_cache();
    let response =
        response_with_headers(&[(CACHE_CONTROL, "max-age=1000"), (SET_COOKIE, "sid=secret")]);
    assert!(cache.store_for_test(&request(), &response, b"body").await);
    assert_eq!(cache.probe(&request()).await, Some(ValidationStatus::Valid));
    assert!(
        !cache
            .headers_for_test(&request())
            .await
            .unwrap()
            .contains_key(SET_COOKIE),
        "a stored `Set-Cookie` would put the cookie on disk and be re-presented on every hit"
    );
}

#[tokio::test]
async fn test_vary_on_cookie_still_selects_by_cookie() {
    // Credentials are digested rather than stored, so the entry has to keep
    // matching the cookie it was stored for, and only that one.
    let cache = test_cache();
    let mut stored = HeaderMap::new();
    stored.insert("cookie", HeaderValue::from_static("sid=one"));
    let stored_request = request_with_headers(stored);
    let response = response_with_headers(&[(CACHE_CONTROL, "max-age=1000"), (VARY, "cookie")]);
    assert!(
        cache
            .store_for_test(&stored_request, &response, b"body")
            .await
    );

    assert_eq!(
        cache.probe(&stored_request).await,
        Some(ValidationStatus::Valid)
    );

    let mut other = HeaderMap::new();
    other.insert("cookie", HeaderValue::from_static("sid=two"));
    assert_eq!(
        cache.probe(&request_with_headers(other)).await,
        None,
        "another session's cookie must not be answered from this entry"
    );
}

#[tokio::test]
async fn test_vary_selects_the_matching_variant() {
    let cache = test_cache();

    let mut gzip_headers = HeaderMap::new();
    gzip_headers.insert("accept-encoding", HeaderValue::from_static("gzip"));
    let gzip_request = request_with_headers(gzip_headers);
    let response =
        response_with_headers(&[(CACHE_CONTROL, "max-age=1000"), (VARY, "accept-encoding")]);
    assert!(
        cache
            .store_for_test(&gzip_request, &response, b"gzip")
            .await
    );

    assert_eq!(
        cache.probe(&gzip_request).await,
        Some(ValidationStatus::Valid)
    );
    let mut br_headers = HeaderMap::new();
    br_headers.insert("accept-encoding", HeaderValue::from_static("br"));
    assert_eq!(
        cache.probe(&request_with_headers(br_headers)).await,
        None,
        "a request whose Vary-nominated headers differ must not match the stored variant"
    );
}

#[tokio::test]
async fn test_expired_response_needs_synchronous_validation() {
    let cache = test_cache();
    let request = request();
    let response = response_with_headers(&[(EXPIRES, "0"), (ETAG, "\"v1\"")]);
    assert!(cache.store_for_test(&request, &response, b"body").await);
    assert_eq!(
        cache.probe(&request).await,
        Some(ValidationStatus::Stale {
            revalidate_in_background: false
        })
    );
}

#[tokio::test]
async fn test_body_is_stored_encoded() {
    let cache = test_cache();
    let request = request();
    let response =
        response_with_headers(&[(CACHE_CONTROL, "max-age=1000"), (CONTENT_ENCODING, "gzip")]);
    let compressed = b"\x1f\x8b not really gzip";
    assert!(cache.store_for_test(&request, &response, compressed).await);
    assert_eq!(
        cache.read_body_for_test(&request).await.as_deref(),
        Some(&compressed[..]),
        "the cache must store the body exactly as it came off the wire"
    );
}

/// Store a response with the given `Cache-Control`, then report the freshness the
/// cache assigns it on a subsequent request.
async fn stale_while_revalidate_status_for(cache_control: &str) -> ValidationStatus {
    let cache = test_cache();
    let request = request_with_client();
    let response = response_with_headers(&[(CACHE_CONTROL, cache_control)]);
    assert!(cache.store_for_test(&request, &response, &[1, 2, 3]).await);
    cache
        .probe(&request)
        .await
        .expect("the stored response should be found")
}

#[tokio::test]
async fn test_stale_within_stale_while_revalidate_window_serves_immediately_and_revalidates_in_background()
 {
    assert_eq!(
        stale_while_revalidate_status_for("max-age=0, stale-while-revalidate=30").await,
        ValidationStatus::Stale {
            revalidate_in_background: true
        },
        "stale response within the stale-while-revalidate window should be served immediately \
         and revalidated in the background"
    );
}

#[tokio::test]
async fn test_stale_without_stale_while_revalidate_requires_synchronous_validation() {
    assert_eq!(
        stale_while_revalidate_status_for("max-age=0").await,
        ValidationStatus::Stale {
            revalidate_in_background: false
        },
        "stale response without stale-while-revalidate must be synchronously revalidated"
    );
}

#[tokio::test]
async fn test_stale_while_revalidate_not_used_when_request_demands_revalidation() {
    let cache = test_cache();
    let response =
        response_with_headers(&[(CACHE_CONTROL, "max-age=0, stale-while-revalidate=30")]);
    assert!(
        cache
            .store_for_test(&request(), &response, &[1, 2, 3])
            .await
    );

    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    assert_eq!(
        cache.probe(&request_with_headers(headers)).await,
        Some(ValidationStatus::Stale {
            revalidate_in_background: false
        }),
        "a no-cache request must trigger synchronous validation, not background revalidation"
    );
}

#[tokio::test]
async fn test_stale_while_revalidate_without_a_client_requires_synchronous_validation() {
    let cache = test_cache();
    let response =
        response_with_headers(&[(CACHE_CONTROL, "max-age=0, stale-while-revalidate=30")]);
    assert!(
        cache
            .store_for_test(&request(), &response, &[1, 2, 3])
            .await
    );
    assert_eq!(
        cache.probe(&request()).await,
        Some(ValidationStatus::Stale {
            revalidate_in_background: false
        }),
        "only a request with a client revalidates in the background"
    );
}

/// The response `cache` gives to a request for `url` with `range`, after storing a
/// response with `status`, `headers` and `body`.
async fn cached_range_response(
    url: &str,
    status: StatusCode,
    headers: &[(HeaderName, &str)],
    body: &[u8],
    range: &str,
) -> Option<(Response, Vec<u8>)> {
    let cache = test_cache();
    let mut response = response_with_headers(&[(CACHE_CONTROL, "max-age=1000")]);
    response.status = status.into();
    for (name, value) in headers {
        response
            .headers
            .insert(name.clone(), HeaderValue::from_str(value).unwrap());
    }
    assert!(
        cache
            .store_for_test(&request_for(url, &[]), &response, body)
            .await
    );
    cache
        .serve_for_test(&request_for(url, &[(RANGE, range)]))
        .await
}

#[tokio::test]
async fn test_range_from_complete_cached_resource_adjusts_headers() {
    let body: Vec<u8> = (0..100).collect();
    let (response, served) = cached_range_response(
        "https://servo.org/complete",
        StatusCode::OK,
        &[(CONTENT_LENGTH, "100")],
        &body,
        "bytes=10-19",
    )
    .await
    .expect("expected a response from the cache");

    assert_eq!(response.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(served, (10u8..20).collect::<Vec<_>>());
    assert_eq!(
        response.headers.get(CONTENT_LENGTH),
        Some(&HeaderValue::from_static("10"))
    );
    assert_eq!(
        response.headers.get(CONTENT_RANGE),
        Some(&HeaderValue::from_static("bytes 10-19/100"))
    );
}

#[tokio::test]
async fn test_range_from_an_encoded_cached_resource_is_left_to_the_network() {
    let response = cached_range_response(
        "https://servo.org/encoded",
        StatusCode::OK,
        &[(CONTENT_ENCODING, "gzip"), (CONTENT_LENGTH, "42")],
        &[0; 42],
        "bytes=10-19",
    )
    .await;
    assert!(
        response.is_none(),
        "ranges refer to the decoded body, but the stored body is encoded"
    );
}

/// A last-byte-pos beyond the end of the representation is clamped to it, rather
/// than treated as a cache miss.
#[tokio::test]
async fn test_range_beyond_end_of_complete_cached_resource_is_clamped() {
    let body: Vec<u8> = (0..100).collect();
    let (response, served) = cached_range_response(
        "https://servo.org/clamped",
        StatusCode::OK,
        &[],
        &body,
        "bytes=90-99999",
    )
    .await
    .expect("expected a response from the cache");

    assert_eq!(served, (90u8..100).collect::<Vec<_>>());
    assert_eq!(
        response.headers.get(CONTENT_RANGE),
        Some(&HeaderValue::from_static("bytes 90-99/100"))
    );
}

/// A suffix range is relative to the complete representation, whose length a
/// stored partial response knows from its `Content-Range`.
#[tokio::test]
async fn test_suffix_range_from_partial_cached_resource() {
    let body: Vec<u8> = (0..100).collect();
    let (response, served) = cached_range_response(
        "https://servo.org/partial",
        StatusCode::PARTIAL_CONTENT,
        &[(CONTENT_RANGE, "bytes 0-99/100")],
        &body,
        "bytes=-10",
    )
    .await
    .expect("expected a response from the cache");

    assert_eq!(served, (90u8..100).collect::<Vec<_>>());
    assert_eq!(
        response.headers.get(CONTENT_RANGE),
        Some(&HeaderValue::from_static("bytes 90-99/100"))
    );
}

/// A suffix range longer than the stored representation is left to the network.
#[tokio::test]
async fn test_unsatisfiable_suffix_range_is_not_served() {
    let complete = cached_range_response(
        "https://servo.org/short",
        StatusCode::OK,
        &[],
        &[1, 2, 3, 4, 5],
        "bytes=-5000",
    )
    .await;
    assert!(complete.is_none());

    let partial = cached_range_response(
        "https://servo.org/short-partial",
        StatusCode::PARTIAL_CONTENT,
        &[(CONTENT_RANGE, "bytes 0-4/5")],
        &[1, 2, 3, 4, 5],
        "bytes=-5000",
    )
    .await;
    assert!(partial.is_none());
}

/// A stored partial response whose `Content-Range` reports an empty
/// representation cannot satisfy any range.
#[tokio::test]
async fn test_partial_cached_resource_with_empty_representation() {
    let cache = test_cache();
    let mut response = response_with_headers(&[
        (CACHE_CONTROL, "max-age=1000"),
        (CONTENT_RANGE, "bytes 0-0/0"),
    ]);
    response.status = StatusCode::PARTIAL_CONTENT.into();
    let url = "https://servo.org/empty";
    // Either it is not stored, or it does not answer.
    let _ = cache
        .store_for_test(&request_for(url, &[]), &response, &[])
        .await;
    assert!(
        cache
            .serve_for_test(&request_for(url, &[(RANGE, "bytes=0-10")]))
            .await
            .is_none()
    );
}

fn media_request(destination: Destination, range: Option<&str>) -> Request {
    let mut request = request();
    request.destination = destination;
    if let Some(range) = range {
        request
            .headers
            .insert(RANGE, HeaderValue::from_str(range).unwrap());
    }
    request
}

/// Store a cacheable response to `request` with `status`, `headers` and a body of
/// `body_length` bytes in an empty cache, and return whether the cache kept it.
async fn is_stored(
    request: &Request,
    status: StatusCode,
    headers: &[(HeaderName, String)],
    body_length: usize,
) -> bool {
    let mut response = response_with_headers(&[(CACHE_CONTROL, "max-age=3600")]);
    response.status = status.into();
    for (name, value) in headers {
        response
            .headers
            .insert(name.clone(), HeaderValue::from_str(value).unwrap());
    }
    let cache = HttpCache::with_store(Arc::new(MemoryStore::new(128 * 1024 * 1024)));
    cache
        .store_for_test(request, &response, &vec![0; body_length])
        .await
}

#[tokio::test]
async fn test_media_responses_are_stored_only_if_complete_and_small() {
    const LIMIT: usize = 8 * 1024 * 1024;
    let video = |range| media_request(Destination::Video, range);
    let audio = |range| media_request(Destination::Audio, range);
    let length = |length: usize| (CONTENT_LENGTH, length.to_string());
    let range = |range: &str| (CONTENT_RANGE, range.to_owned());
    let ok = StatusCode::OK;
    let partial = StatusCode::PARTIAL_CONTENT;

    assert!(is_stored(&video(None), ok, &[length(10)], 10).await);
    assert!(is_stored(&video(Some("bytes=0-")), ok, &[length(10)], 10).await);
    assert!(
        is_stored(
            &video(Some("bytes=0-")),
            partial,
            &[length(10), range("bytes 0-9/10")],
            10
        )
        .await
    );
    assert!(
        is_stored(&video(Some("bytes=5-")), ok, &[length(10)], 10).await,
        "a 200 holds the whole resource, whatever range was requested"
    );
    assert!(is_stored(&audio(None), ok, &[length(LIMIT)], LIMIT).await);
    assert!(
        is_stored(
            &video(Some("bytes=0-")),
            partial,
            &[range("bytes 0-9/10")],
            10
        )
        .await,
        "Content-Range gives the size of a 206"
    );
    assert!(
        is_stored(&video(None), StatusCode::NOT_FOUND, &[], 10).await,
        "statuses other than 200 and 206 follow the usual rules"
    );

    assert!(!is_stored(&audio(None), ok, &[length(LIMIT + 1)], LIMIT + 1).await);
    assert!(
        !is_stored(&video(None), ok, &[], 10).await,
        "the length of a chunked response is not known up front"
    );
    assert!(
        !is_stored(
            &video(None),
            ok,
            &[length(10), (CONTENT_ENCODING, "gzip".to_owned())],
            10
        )
        .await,
        "Content-Length is not the length of a decoded body"
    );
    assert!(
        !is_stored(
            &video(Some("bytes=5-")),
            partial,
            &[length(5), range("bytes 5-9/10")],
            5
        )
        .await
    );
    assert!(
        !is_stored(
            &video(Some("bytes=0-")),
            partial,
            &[length(5), range("bytes 0-4/10")],
            5
        )
        .await
    );
    assert!(
        !is_stored(
            &video(Some("bytes=0-")),
            partial,
            &[length(10), range("bytes 0-9/*")],
            10
        )
        .await
    );
    assert!(
        !is_stored(
            &video(Some("bytes=0-")),
            partial,
            &[range(&format!("bytes 0-{LIMIT}/{}", LIMIT + 1))],
            LIMIT + 1
        )
        .await
    );

    let other = |range| media_request(Destination::None, range);
    assert!(
        is_stored(
            &other(Some("bytes=5-")),
            partial,
            &[length(5), range("bytes 5-9/10")],
            5
        )
        .await
    );
    assert!(is_stored(&other(None), ok, &[], LIMIT + 1).await);
}

#[tokio::test]
async fn test_invalidation_only_reaches_the_same_origin() {
    let cache = test_cache();
    let fresh = response_with_headers(&[(CACHE_CONTROL, "max-age=1000")]);
    let same_origin = "https://servo.org/same";
    let other_origin = "https://example.org/other";
    for url in [URL, same_origin, other_origin] {
        assert!(
            cache
                .store_for_test(&request_for(url, &[]), &fresh, b"body")
                .await
        );
    }

    let mut post = request();
    post.method = Method::POST;
    let mut response = response_with_headers(&[]);
    response
        .headers
        .insert(LOCATION, HeaderValue::from_static(other_origin));
    response
        .headers
        .insert(CONTENT_LOCATION, HeaderValue::from_static("/same"));
    cache.invalidate_for_test(&post, &response).await;

    assert_eq!(
        cache.probe(&request()).await,
        None,
        "the target is invalidated"
    );
    assert_eq!(
        cache.probe(&request_for(same_origin, &[])).await,
        None,
        "a same-origin Content-Location is invalidated"
    );
    assert_eq!(
        cache.probe(&request_for(other_origin, &[])).await,
        Some(ValidationStatus::Valid),
        "RFC 9111 section 4.4: a cache MUST NOT invalidate a URI of another origin"
    );
}

#[tokio::test]
async fn test_the_most_recent_matching_response_is_used() {
    let cache = test_cache();
    let request = request();
    for (date, version) in [
        ("Thu, 01 Oct 2026 10:00:00 GMT", "older"),
        ("Thu, 01 Oct 2026 11:00:00 GMT", "newer"),
    ] {
        let mut response = response_with_headers(&[
            (CACHE_CONTROL, "max-age=100000000"),
            (DATE, date),
            (ETAG, version),
        ]);
        response.headers.insert(
            HeaderName::from_static("x-version"),
            HeaderValue::from_static(version),
        );
        assert!(cache.store_for_test(&request, &response, b"body").await);
    }
    let headers = cache.headers_for_test(&request).await.unwrap();
    assert_eq!(
        headers.get("x-version"),
        Some(&HeaderValue::from_static("newer")),
        "RFC 9111 section 4: a cache MUST use the most recent suitable response"
    );
}
