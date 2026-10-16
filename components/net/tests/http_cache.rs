/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::atomic::Ordering;

use http::header::{
    CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, EXPIRES, HeaderName,
    HeaderValue, RANGE,
};
use http::{HeaderMap, StatusCode};
use net::http_cache::{CacheKey, HttpCache, HttpCacheAssignment, ValidationStatus, refresh};
use net_traits::blob_url_store::UrlWithBlobClaim;
use net_traits::request::{Destination, Referrer, Request, RequestBuilder};
use net_traits::response::{Response, ResponseBody};
use net_traits::{ResourceFetchTiming, ResourceTimingType};
use servo_base::id::TEST_PIPELINE_ID;
use servo_config::prefs::Preferences;
use servo_url::ServoUrl;
use tokio::sync::mpsc::unbounded_channel as unbounded;

#[tokio::test]
async fn test_refreshing_resource_sets_done_chan_the_appropriate_value() {
    let response_bodies = vec![
        ResponseBody::Receiving(vec![]),
        ResponseBody::Empty,
        ResponseBody::Done(vec![]),
    ];
    let url = ServoUrl::parse("https://servo.org").unwrap();
    let request = RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url.clone(), None),
        Referrer::NoReferrer,
    )
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .origin(url.origin())
    .build();
    let timing = ResourceFetchTiming::new(ResourceTimingType::Navigation);
    let mut response = Response::new(url.clone(), timing);
    // Expires header makes the response cacheable.
    response
        .headers
        .insert(EXPIRES, HeaderValue::from_str("-10").unwrap());
    let cache = HttpCache::new(HttpCacheAssignment::Public);
    for body in response_bodies {
        *response.body.lock() = body.clone();
        // First, store the 'normal' response.
        let mut resource = {
            let guard = cache.get_or_guard(CacheKey::new(&request)).await;
            guard.insert(&request, &response);
            cache.get_or_guard(CacheKey::new(&request)).await
        };
        // Second, mutate the response into a 304 response, and refresh the stored one.
        response.status = StatusCode::NOT_MODIFIED.into();
        let (send, recv) = unbounded();
        let mut done_chan = Some((send, recv));
        let refreshed_response = refresh(
            &request,
            response.clone(),
            &mut done_chan,
            resource.try_as_mut().unwrap(),
        );
        // Ensure a resource was found, and refreshed.
        assert!(refreshed_response.is_some());
        match body {
            ResponseBody::Receiving(_) => assert!(done_chan.is_some()),
            ResponseBody::Empty | ResponseBody::Done(_) => assert!(done_chan.is_none()),
        }
    }
}

#[tokio::test]
async fn test_skip_incomplete_cache_for_range_request_with_no_end_bound() {
    let actual_body_len = 10;
    let incomplete_response_body = &[1, 2, 3, 4, 5];
    let url = ServoUrl::parse("https://servo.org").unwrap();

    let cache = HttpCache::new(HttpCacheAssignment::Public);
    let mut headers = HeaderMap::new();

    headers.insert(
        RANGE,
        HeaderValue::from_str(&format!("bytes={}-", 0)).unwrap(),
    );
    let request = RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url.clone(), None),
        Referrer::NoReferrer,
    )
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .origin(url.origin())
    .headers(headers)
    .build();

    // Store incomplete response to http_cache
    let timing = ResourceFetchTiming::new(ResourceTimingType::Navigation);
    let mut initial_incomplete_response = Response::new(url.clone(), timing);
    *initial_incomplete_response.body.lock() =
        ResponseBody::Done(incomplete_response_body.to_vec());
    initial_incomplete_response.headers.insert(
        CONTENT_RANGE,
        HeaderValue::from_str(&format!(
            "bytes 0-{}/{}",
            actual_body_len - 1,
            actual_body_len
        ))
        .unwrap(),
    );
    initial_incomplete_response.headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&format!("{}", actual_body_len)).unwrap(),
    );
    initial_incomplete_response
        .headers
        .insert(EXPIRES, HeaderValue::from_str("0").unwrap());
    initial_incomplete_response.status = StatusCode::PARTIAL_CONTENT.into();
    cache.store(&request, &initial_incomplete_response).await;

    // Try to construct response from http_cache
    let mut done_chan = None;
    let consecutive_response = cache.construct_response(&request, &mut done_chan).await;
    assert!(
        consecutive_response.is_none(),
        "Should not construct response from incomplete response!"
    );
}

fn build_stale_while_revalidate_test_request() -> net_traits::request::Request {
    let url = ServoUrl::parse("https://servo.org").unwrap();
    RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url.clone(), None),
        Referrer::NoReferrer,
    )
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .origin(url.origin())
    .build()
}

/// Store a response with the given `Cache-Control` value, then return the
/// freshness state [`ValidationStatus`] reported by the cache for a subsequent request.
async fn stale_while_revalidate_freshness_for_cache_control(
    cache_control: &str,
) -> ValidationStatus {
    let url = ServoUrl::parse("https://servo.org").unwrap();
    let request = build_stale_while_revalidate_test_request();

    let timing = ResourceFetchTiming::new(ResourceTimingType::Navigation);
    let mut response = Response::new(url, timing);
    *response.body.lock() = ResponseBody::Done(vec![1, 2, 3]);
    response
        .headers
        .insert(CACHE_CONTROL, HeaderValue::from_str(cache_control).unwrap());

    let cache = HttpCache::new(HttpCacheAssignment::Public);
    cache.store(&request, &response).await;

    let mut done_chan = None;
    cache
        .construct_response_freshness(&request, &mut done_chan)
        .await
        .expect("a response should be constructable from the cache")
}

#[tokio::test]
async fn test_stale_within_stale_while_revalidate_window_serves_immediately_and_revalidates_in_background()
 {
    let validation_status =
        stale_while_revalidate_freshness_for_cache_control("max-age=0, stale-while-revalidate=30")
            .await;
    assert_eq!(
        validation_status,
        ValidationStatus::Stale {
            revalidate_in_background: true
        },
        "stale response within the stale-while-revalidate window should be served immediately \
         and revalidated in the background"
    );
}

#[tokio::test]
async fn test_stale_without_stale_while_revalidate_requires_synchronous_validation() {
    let validation_status = stale_while_revalidate_freshness_for_cache_control("max-age=0").await;
    assert_eq!(
        validation_status,
        ValidationStatus::Stale {
            revalidate_in_background: false
        },
        "stale response without stale-while-revalidate must be synchronously revalidated"
    );
}

#[tokio::test]
async fn test_stale_while_revalidate_not_used_when_request_demands_revalidation() {
    let url = ServoUrl::parse("https://servo.org").unwrap();

    let timing = ResourceFetchTiming::new(ResourceTimingType::Navigation);
    let mut response = Response::new(url.clone(), timing);
    *response.body.lock() = ResponseBody::Done(vec![1, 2, 3]);
    response.headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_str("max-age=0, stale-while-revalidate=30").unwrap(),
    );

    let store_request = build_stale_while_revalidate_test_request();
    let cache = HttpCache::new(HttpCacheAssignment::Public);
    cache.store(&store_request, &response).await;

    let mut req_headers = HeaderMap::new();
    req_headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    let request = RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url.clone(), None),
        Referrer::NoReferrer,
    )
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .origin(url.origin())
    .headers(req_headers)
    .build();

    let mut done_chan = None;
    let validation_status = cache
        .construct_response_freshness(&request, &mut done_chan)
        .await
        .expect("a response should be constructable from the cache");
    assert_eq!(
        validation_status,
        ValidationStatus::Stale {
            revalidate_in_background: false
        },
        "a no-cache request must trigger synchronous validation, not background revalidation"
    );
}

#[tokio::test]
async fn http_cache_get_items() {
    servo_config::prefs::set(Preferences {
        network_http_cache_size: 4,
        ..Default::default()
    });
    let cache = HttpCache::new(HttpCacheAssignment::Public);
    let url = ServoUrl::parse(&format!("https://www.servo.org/really_long_request")).unwrap();
    let request = RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url.clone(), None),
        Referrer::NoReferrer,
    )
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .origin(url.origin())
    .build();
    let timing = ResourceFetchTiming::new(ResourceTimingType::Navigation);
    let mut response = Response::new(url, timing);
    response
        .headers
        .insert(CACHE_CONTROL, HeaderValue::from_static("max-age=36000"));
    *response.body.lock() = ResponseBody::Done(vec![1, 2, 3]);
    cache.store(&request, &response).await;

    let cache_result = cache.construct_response(&request, &mut None).await;

    assert!(cache_result.is_some());
    let cache_result = cache_result.unwrap();
    assert_eq!(
        &*cache_result.body.lock(),
        &ResponseBody::Done(vec![1, 2, 3])
    );
}

#[tokio::test]
async fn http_cache_size() {
    servo_config::prefs::set(Preferences {
        network_http_cache_size: 4,
        ..Default::default()
    });
    let cache = HttpCache::new(HttpCacheAssignment::Public);

    // Fill requests that are not done
    for i in 0..4 {
        let url = ServoUrl::parse(&format!("https://www.servo.org/{i}")).unwrap();
        let request = RequestBuilder::new(
            None,
            UrlWithBlobClaim::new(url.clone(), None),
            Referrer::NoReferrer,
        )
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .origin(url.origin())
        .build();
        let timing = ResourceFetchTiming::new(ResourceTimingType::Navigation);
        let mut response = Response::new(url, timing);
        response
            .headers
            .insert(CACHE_CONTROL, HeaderValue::from_static("max-age=36000"));
        *response.body.lock() = ResponseBody::Done(vec![1, 2, 3]);

        cache.store(&request, &response).await;
    }

    assert_eq!(cache.len(), 4);
}

#[tokio::test]
async fn http_cache_do_not_evict_unfinished_requests() {
    servo_config::prefs::set(Preferences {
        network_http_cache_size: 4,
        ..Default::default()
    });
    let cache = HttpCache::new(HttpCacheAssignment::Public);

    // Store a not yet finished request
    let not_finished_request = {
        let url = ServoUrl::parse(&format!("https://www.servo.org/really_long_request")).unwrap();
        let request = RequestBuilder::new(
            None,
            UrlWithBlobClaim::new(url.clone(), None),
            Referrer::NoReferrer,
        )
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .origin(url.origin())
        .build();
        let timing = ResourceFetchTiming::new(ResourceTimingType::Navigation);
        let mut response = Response::new(url, timing);
        response
            .headers
            .insert(CACHE_CONTROL, HeaderValue::from_static("max-age=36000"));
        *response.body.lock() = ResponseBody::Receiving(vec![1, 2, 3]);
        cache.store(&request, &response).await;
        request
    };

    // Fill requests that are not done
    for i in 0..12 {
        let url = ServoUrl::parse(&format!("https://www.servo.org/{i}")).unwrap();
        let request = RequestBuilder::new(
            None,
            UrlWithBlobClaim::new(url.clone(), None),
            Referrer::NoReferrer,
        )
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .origin(url.origin())
        .build();
        let timing = ResourceFetchTiming::new(ResourceTimingType::Navigation);
        let mut response = Response::new(url, timing);
        response
            .headers
            .insert(CACHE_CONTROL, HeaderValue::from_static("max-age=36000"));
        *response.body.lock() = ResponseBody::Done(vec![1, 2, 3]);

        cache.store(&request, &response).await;
    }

    let result = cache
        .construct_response(&not_finished_request, &mut None)
        .await;
    println!("cache {:?}", cache);
    assert!(result.is_some());
}

fn build_request_with_range(destination: Destination, range: Option<&str>) -> Request {
    let url = ServoUrl::parse("https://servo.org/video.mp4").unwrap();
    let mut headers = HeaderMap::new();
    if let Some(range) = range {
        headers.insert(RANGE, HeaderValue::from_str(range).unwrap());
    }
    RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url.clone(), None),
        Referrer::NoReferrer,
    )
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .origin(url.origin())
    .destination(destination)
    .headers(headers)
    .build()
}

/// Store a cacheable response to `request` with `status`, `headers` and a body of `body_length`
/// bytes in an empty cache, and return whether the cache kept it.
async fn is_stored(
    request: &Request,
    status: StatusCode,
    headers: &[(HeaderName, String)],
    body_length: usize,
) -> bool {
    let timing = ResourceFetchTiming::new(ResourceTimingType::Resource);
    let mut response = Response::new(request.current_url(), timing);
    response.status = status.into();
    response
        .headers
        .insert(CACHE_CONTROL, HeaderValue::from_static("max-age=3600"));
    for (name, value) in headers {
        response
            .headers
            .insert(name.clone(), HeaderValue::from_str(value).unwrap());
    }
    *response.body.lock() = ResponseBody::Done(vec![0; body_length]);
    let cache = HttpCache::new(HttpCacheAssignment::Public);
    cache.store(request, &response).await;
    cache.stored_response_count(request).await == 1
}

#[tokio::test]
async fn test_media_responses_are_stored_only_if_complete_and_small() {
    const LIMIT: usize = 8 * 1024 * 1024;
    let video = |range| build_request_with_range(Destination::Video, range);
    let audio = |range| build_request_with_range(Destination::Audio, range);
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

    let other = |range| build_request_with_range(Destination::None, range);
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

fn build_cacheable_response(request: &Request, body: ResponseBody) -> Response {
    let timing = ResourceFetchTiming::new(ResourceTimingType::Resource);
    let mut response = Response::new(request.current_url(), timing);
    response
        .headers
        .insert(CACHE_CONTROL, HeaderValue::from_static("max-age=3600"));
    *response.body.lock() = body;
    response
}

#[tokio::test]
async fn test_aborted_responses_are_removed_when_storing() {
    let request = build_request_with_range(Destination::None, None);
    let build_response =
        |body: &[u8]| build_cacheable_response(&request, ResponseBody::Done(body.to_vec()));
    let cache = HttpCache::new(HttpCacheAssignment::Public);

    let aborted_response = build_response(b"aborted");
    cache.store(&request, &aborted_response).await;
    aborted_response.aborted.store(true, Ordering::Release);
    cache.store(&request, &build_response(b"complete")).await;

    assert_eq!(cache.stored_response_count(&request).await, 1);
    let cached_response = cache.construct_response(&request, &mut None).await.unwrap();
    assert_eq!(
        *cached_response.body.lock(),
        ResponseBody::Done(b"complete".to_vec())
    );
}

#[tokio::test]
async fn test_aborted_responses_are_kept_while_consumers_wait_for_them() {
    let request = build_request_with_range(Destination::None, None);
    let cache = HttpCache::new(HttpCacheAssignment::Public);

    let aborted_response = build_cacheable_response(&request, ResponseBody::Receiving(vec![]));
    cache.store(&request, &aborted_response).await;
    let mut done_chan = None;
    assert!(
        cache
            .construct_response(&request, &mut done_chan)
            .await
            .is_some()
    );
    assert!(done_chan.is_some());
    aborted_response.aborted.store(true, Ordering::Release);

    let response = build_cacheable_response(&request, ResponseBody::Done(b"complete".to_vec()));
    cache.store(&request, &response).await;
    assert_eq!(cache.stored_response_count(&request).await, 2);
}

#[tokio::test]
async fn test_ranges_of_a_stored_response_share_its_aborted_flag() {
    let request = build_request_with_range(Destination::None, Some("bytes=2-"));
    let response = build_cacheable_response(&request, ResponseBody::Done(b"0123456789".to_vec()));
    let cache = HttpCache::new(HttpCacheAssignment::Public);
    cache.store(&request, &response).await;

    let range_response = cache.construct_response(&request, &mut None).await.unwrap();
    assert_eq!(
        *range_response.body.lock(),
        ResponseBody::Done(b"23456789".to_vec())
    );
    response.aborted.store(true, Ordering::Release);
    assert!(range_response.aborted.load(Ordering::Acquire));
}

#[tokio::test]
async fn test_refresh_skips_aborted_responses() {
    let request = build_request_with_range(Destination::None, None);
    let cache = HttpCache::new(HttpCacheAssignment::Public);
    let aborted_response = build_cacheable_response(&request, ResponseBody::Done(vec![]));
    cache.store(&request, &aborted_response).await;
    let response = build_cacheable_response(&request, ResponseBody::Done(b"complete".to_vec()));
    cache.store(&request, &response).await;
    aborted_response.aborted.store(true, Ordering::Release);

    let mut not_modified = build_cacheable_response(&request, ResponseBody::Empty);
    not_modified.status = StatusCode::NOT_MODIFIED.into();
    let mut resources = cache.get_or_guard(CacheKey::new(&request)).await;
    let refreshed_response = refresh(
        &request,
        not_modified,
        &mut None,
        resources.try_as_mut().unwrap(),
    )
    .unwrap();
    assert_eq!(
        *refreshed_response.body.lock(),
        ResponseBody::Done(b"complete".to_vec())
    );
}
