/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

use content_security_policy as csp;
use cookie::Cookie as CookiePair;
use crossbeam_channel::{Receiver, unbounded};
use devtools_traits::{
    ChromeToDevtoolsControlMsg, DevtoolsControlMsg, HttpRequest as DevtoolsHttpRequest,
    HttpResponse as DevtoolsHttpResponse, NetworkEvent,
};
use embedder_traits::AuthenticationResponse;
use flate2::Compression;
use flate2::write::{GzEncoder, ZlibEncoder};
use futures::stream;
use headers::authorization::Basic;
use headers::{
    Authorization, ContentLength, Date, HeaderMapExt, Host, StrictTransportSecurity, UserAgent,
};
use http::header::{self, HeaderMap, HeaderValue, WWW_AUTHENTICATE};
use http::uri::Authority;
use http::{HeaderName, Method, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Body, Bytes, Frame, Incoming};
use hyper::{Request as HyperRequest, Response as HyperResponse};
use net::async_runtime::spawn_task;
use net::connector::CACertificates;
use net::cookie::ServoCookie;
use net::cookie_storage::CookieStorage;
use net::fetch::methods::{self, CancellationListener, FetchContext, SuspendedFetches};
use net::http_loader::{determine_requests_referrer, serialize_origin};
use net::protocols::ProtocolRegistry;
use net::resource_thread::{AuthCacheEntry, new_core_resource_thread};
use net::test::DECODER_BUFFER_SIZE;
use net::test_util::make_h2_ssl_server;
use net_traits::blob_url_store::UrlWithBlobClaim;
use net_traits::fetch::fetch_controller::FetchController;
use net_traits::http_status::HttpStatus;
use net_traits::request::{
    CredentialsMode, Destination, Referrer, Request, RequestBuilder, RequestMode,
    TraversableForUserPrompts, create_request_body_with_content,
};
use net_traits::response::{CacheState, Response, ResponseBody};
use net_traits::{
    CookieSource, CoreResourceMsg, CoreResourceThread, FetchResponseMsg, FetchTaskTarget,
    NetworkError, ReferrerPolicy, fetch_async, get_current_locale,
};
use parking_lot::{Mutex, RwLock};
use profile_traits::mem::ProfilerChan as MemProfilerChan;
use profile_traits::time::ProfilerChan;
use servo_base::generic_channel;
use servo_base::id::{TEST_PIPELINE_ID, TEST_WEBVIEW_ID};
use servo_url::{ImmutableOrigin, ServoUrl};
use url::Url;

use crate::{
    create_generic_embedder_proxy, create_generic_embedder_proxy_and_receiver, create_http_state,
    fetch, fetch_with_context, make_body, make_server, make_ssl_server, mock_origin,
    new_fetch_context, receive_credential_prompt_msgs, replace_host_table, spawn_blocking_task,
};

fn assert_cookie_for_domain(
    cookie_jar: &RwLock<CookieStorage>,
    domain: &str,
    cookie: Option<&str>,
) {
    let mut cookie_jar = cookie_jar.write();
    let url = ServoUrl::parse(&*domain).unwrap();
    let cookies = cookie_jar.cookies_for_url(&url, CookieSource::HTTP);
    assert_eq!(cookies.as_ref().map(|c| &**c), cookie);
}

fn recv_http_request(devtools_port: &Receiver<DevtoolsControlMsg>) -> DevtoolsHttpRequest {
    match devtools_port.recv().unwrap() {
        DevtoolsControlMsg::FromChrome(ChromeToDevtoolsControlMsg::NetworkEvent(_, net_event)) => {
            match net_event {
                NetworkEvent::HttpRequest(req) => req,
                NetworkEvent::HttpRequestUpdate(req) => req,
                other => panic!("Expected HttpRequest but got: {:?}", other),
            }
        },
        other => panic!("Expected NetworkEvent but got: {:?}", other),
    }
}

fn recv_all_network_events(devtools_port: Receiver<DevtoolsControlMsg>) -> Vec<NetworkEvent> {
    let mut events = vec![];
    while let Ok(msg) = devtools_port.recv() {
        match msg {
            DevtoolsControlMsg::FromChrome(ChromeToDevtoolsControlMsg::NetworkEvent(
                _,
                net_event,
            )) => events.push(net_event),
            other => panic!("Expected NetworkEvent but got: {:?}", other),
        }
    }
    events
}

pub fn expect_devtools_http_request(
    devtools_port: &Receiver<DevtoolsControlMsg>,
) -> (DevtoolsHttpRequest, DevtoolsHttpRequest) {
    (
        recv_http_request(devtools_port),
        recv_http_request(devtools_port),
    )
}

fn pluck<F: Fn(&NetworkEvent) -> bool>(events: &mut Vec<NetworkEvent>, f: F) -> NetworkEvent {
    let Some(idx) = events.iter().position(f) else {
        panic!("No matching network event")
    };
    events.remove(idx)
}

#[track_caller]
fn expect_request(events: &mut Vec<NetworkEvent>) -> DevtoolsHttpRequest {
    let event = pluck(events, |event| {
        matches!(event, NetworkEvent::HttpRequest(_))
    });
    match event {
        NetworkEvent::HttpRequest(req) => req,
        _ => unreachable!(),
    }
}

#[track_caller]
fn expect_request_update(events: &mut Vec<NetworkEvent>) -> DevtoolsHttpRequest {
    let event = pluck(events, |event| {
        matches!(event, NetworkEvent::HttpRequestUpdate(_))
    });
    match event {
        NetworkEvent::HttpRequestUpdate(req) => req,
        _ => unreachable!(),
    }
}

#[track_caller]
fn expect_response(events: &mut Vec<NetworkEvent>) -> DevtoolsHttpResponse {
    let event = pluck(events, |event| {
        matches!(event, NetworkEvent::HttpResponse(_))
    });
    match event {
        NetworkEvent::HttpResponse(resp) => resp,
        _ => unreachable!(),
    }
}

pub fn expect_devtools_http_response(
    devtools_port: &Receiver<DevtoolsControlMsg>,
) -> DevtoolsHttpResponse {
    loop {
        match devtools_port.recv().unwrap() {
            DevtoolsControlMsg::FromChrome(ChromeToDevtoolsControlMsg::NetworkEvent(
                _,
                net_event_response,
            )) => match net_event_response {
                NetworkEvent::HttpResponse(httpresponse) => return httpresponse,
                NetworkEvent::SecurityInfo(_) => continue,
                other => panic!("Expected HttpResponse but got: {:?}", other),
            },
            other => panic!("Expected NetworkEvent but got: {:?}", other),
        }
    }
}

pub fn devtools_response_with_body(
    devtools_port: &Receiver<DevtoolsControlMsg>,
) -> DevtoolsHttpResponse {
    let devhttpresponses = vec![
        expect_devtools_http_response(devtools_port),
        expect_devtools_http_response(devtools_port),
    ];
    return devhttpresponses
        .into_iter()
        .find(|resp| resp.body.is_some())
        .expect("One of the responses should have a body");
}

#[test]
fn test_check_default_headers_loaded_in_every_request() {
    let expected_headers = Arc::new(Mutex::new(None));
    let expected_headers_clone = expected_headers.clone();
    let handler = move |request: HyperRequest<Incoming>,
                        _: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
        assert_eq!(
            request.headers().clone(),
            expected_headers_clone.lock().take().unwrap()
        );
    };
    let (server, url) = make_server(handler);

    let mut headers = HeaderMap::new();

    headers.insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("gzip, deflate, br, zstd"),
    );

    headers.typed_insert(Host::from(
        format!("{}:{}", url.host_str().unwrap(), url.port().unwrap())
            .parse::<Authority>()
            .unwrap(),
    ));

    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
    );

    headers.insert(header::ACCEPT_LANGUAGE, get_current_locale().1.clone());

    headers.typed_insert::<UserAgent>(crate::DEFAULT_USER_AGENT.parse().unwrap());

    // Append fetch metadata headers
    headers.insert(
        HeaderName::from_static("sec-fetch-dest"),
        HeaderValue::from_static("document"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-mode"),
        HeaderValue::from_static("no-cors"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-site"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-user"),
        HeaderValue::from_static("?1"),
    );

    *expected_headers.lock() = Some(headers.clone());

    // Testing for method.GET
    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .destination(Destination::Document)
        .origin(url.clone().origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = dbg!(fetch(request, None));
    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );

    // Testing for method.POST
    let mut post_headers = headers.clone();
    post_headers.typed_insert(ContentLength(0 as u64));
    let url_str = url.as_str();
    // request gets header "Origin: http://example.com" but expected_headers has
    // "Origin: http://example.com/" which do not match for equality so strip trailing '/'
    post_headers.insert(
        header::ORIGIN,
        HeaderValue::from_str(&url_str[..url_str.len() - 1]).unwrap(),
    );
    *expected_headers.lock() = Some(post_headers);
    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::POST)
        .destination(Destination::Document)
        .origin(url.clone().origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);
    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );

    let _ = server.close();
}

#[test]
fn test_load_when_request_is_not_get_or_head_and_there_is_no_body_content_length_should_be_set_to_0()
 {
    let handler = move |request: HyperRequest<Incoming>,
                        _: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
        assert_eq!(
            request.headers().typed_get::<ContentLength>(),
            Some(ContentLength(0))
        );
    };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::POST)
        .body(None)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);
    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );

    let _ = server.close();
}

#[test]
fn test_request_and_response_data_with_network_messages() {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response
                .headers_mut()
                .typed_insert(Host::from("foo.bar".parse::<Authority>().unwrap()));
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let mut request_headers = HeaderMap::new();
    request_headers.typed_insert(Host::from("bar.foo".parse::<Authority>().unwrap()));
    let request = RequestBuilder::new(Some(TEST_WEBVIEW_ID), url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .headers(request_headers)
        .body(None)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let (devtools_chan, devtools_port) = unbounded();
    let response = fetch(request, Some(devtools_chan));
    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );

    let _ = server.close();

    // notification received from devtools
    let devhttprequests = expect_devtools_http_request(&devtools_port);
    let devhttpresponse = devtools_response_with_body(&devtools_port);

    // Creating default headers for request
    let mut headers = HeaderMap::new();

    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
    );

    headers.insert(header::ACCEPT_LANGUAGE, get_current_locale().1.clone());

    headers.typed_insert::<UserAgent>(crate::DEFAULT_USER_AGENT.parse().unwrap());

    headers.insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("gzip, deflate, br, zstd"),
    );

    // Append fetch metadata headers
    headers.insert(
        HeaderName::from_static("sec-fetch-dest"),
        HeaderValue::from_static("document"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-mode"),
        HeaderValue::from_static("no-cors"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-site"),
        HeaderValue::from_static("cross-site"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-user"),
        HeaderValue::from_static("?1"),
    );

    let httprequest = DevtoolsHttpRequest {
        url: url.url(),
        method: Method::GET,
        headers: headers,
        body: Some(vec![].into()),
        pipeline_id: TEST_PIPELINE_ID,
        started_date_time: devhttprequests.1.started_date_time,
        time_stamp: devhttprequests.1.time_stamp,
        connect_time: devhttprequests.1.connect_time,
        send_time: devhttprequests.1.send_time,
        destination: Destination::Document,
        is_xhr: false,
        browsing_context_id: TEST_WEBVIEW_ID.into(),
    };

    let content = "Yay!";
    let mut response_headers = HeaderMap::new();
    response_headers.typed_insert(ContentLength(content.len() as u64));
    response_headers.typed_insert(Host::from("foo.bar".parse::<Authority>().unwrap()));
    response_headers.typed_insert(
        devhttpresponse
            .headers
            .as_ref()
            .unwrap()
            .typed_get::<Date>()
            .unwrap()
            .clone(),
    );

    let httpresponse = DevtoolsHttpResponse {
        headers: Some(response_headers),
        status: HttpStatus::default(),
        body: Some(content.as_bytes().to_vec().into()),
        from_cache: false,
        pipeline_id: TEST_PIPELINE_ID,
        browsing_context_id: TEST_WEBVIEW_ID.into(),
    };

    assert_eq!(devhttprequests.1, httprequest);
    assert_eq!(devhttpresponse, httpresponse);
}

#[test]
fn test_request_and_response_message_from_devtool_without_pipeline_id() {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response
                .headers_mut()
                .typed_insert(Host::from("foo.bar".parse::<Authority>().unwrap()));
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(None)
        .policy_container(Default::default())
        .build();

    let (devtools_chan, devtools_port) = unbounded();
    let response = fetch(request, Some(devtools_chan));
    assert!(response.actual_response().status.code().is_success());

    let _ = server.close();

    // notification received from devtools
    assert!(devtools_port.try_recv().is_err());
}

#[test]
fn test_redirected_request_to_devtools() {
    let post_handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(request.method(), Method::GET);
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (post_server, post_url) = make_server(post_handler);

    let post_redirect_url = post_url.clone();
    let pre_handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(request.method(), Method::POST);
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&post_redirect_url.to_string()).unwrap(),
            );
            *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
        };
    let (pre_server, pre_url) = make_server(pre_handler);

    let request = RequestBuilder::new(Some(TEST_WEBVIEW_ID), pre_url.clone(), Referrer::NoReferrer)
        .method(Method::POST)
        .destination(Destination::Document)
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .origin(mock_origin())
        .policy_container(Default::default())
        .build();

    let (devtools_chan, devtools_port) = unbounded();
    fetch(request, Some(devtools_chan));

    let _ = pre_server.close();
    let _ = post_server.close();

    let mut events = recv_all_network_events(devtools_port);
    let first_request = expect_request(&mut events);
    let first_request_update = expect_request_update(&mut events);
    let first_response = expect_response(&mut events);

    assert_eq!(first_request.method, Method::POST);
    assert_eq!(first_request.url, pre_url.url());
    assert_eq!(
        first_response.status,
        HttpStatus::from(StatusCode::MOVED_PERMANENTLY)
    );
    assert_eq!(first_request.method, first_request_update.method);
    assert_eq!(first_request.url, first_request_update.url);

    let second_request = expect_request(&mut events);
    let second_request_update = expect_request_update(&mut events);
    let second_response = expect_response(&mut events);

    assert_eq!(second_request.method, Method::GET);
    assert_eq!(second_request.url, post_url.url());
    assert_eq!(second_response.status, HttpStatus::default());
    assert_eq!(second_request.method, second_request_update.method);
    assert_eq!(second_request.url, second_request_update.url);
}

#[test]
fn test_load_when_redirecting_from_a_post_should_rewrite_next_request_as_get() {
    let post_handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(request.method(), Method::GET);
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (post_server, post_url) = make_server(post_handler);

    let post_redirect_url = post_url.clone();
    let pre_handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(request.method(), Method::POST);
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&post_redirect_url.to_string()).unwrap(),
            );
            *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
        };
    let (pre_server, pre_url) = make_server(pre_handler);

    let request = RequestBuilder::new(None, pre_url.clone(), Referrer::NoReferrer)
        .method(Method::POST)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = pre_server.close();
    let _ = post_server.close();

    assert!(response.to_actual().status.code().is_success());
}

#[test]
fn test_load_should_decode_the_response_as_deflate_when_response_headers_have_content_encoding_deflate()
 {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response.headers_mut().insert(
                header::CONTENT_ENCODING,
                HeaderValue::from_static("deflate"),
            );
            let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
            e.write(b"Yay!").unwrap();
            let encoded_content = e.finish().unwrap();
            *response.body_mut() = make_body(encoded_content);
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = server.close();

    let internal_response = response.internal_response.unwrap();
    assert!(internal_response.status.clone().code().is_success());
    assert_eq!(
        *internal_response.body.lock(),
        ResponseBody::Done(b"Yay!".to_vec())
    );
}

#[test]
fn test_load_should_decode_the_response_as_gzip_when_response_headers_have_content_encoding_gzip() {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response
                .headers_mut()
                .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            let mut e = GzEncoder::new(Vec::new(), Compression::default());
            e.write(b"Yay!").unwrap();
            let encoded_content = e.finish().unwrap();
            *response.body_mut() = make_body(encoded_content);
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = server.close();

    let internal_response = response.internal_response.unwrap();
    assert!(internal_response.status.clone().code().is_success());
    assert_eq!(
        *internal_response.body.lock(),
        ResponseBody::Done(b"Yay!".to_vec())
    );
}

#[test]
fn test_load_doesnt_send_request_body_on_any_redirect() {
    let post_handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(request.method(), Method::GET);
            assert_eq!(request.size_hint().exact(), Some(0));
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (post_server, post_url) = make_server(post_handler);

    let post_redirect_url = post_url.clone();
    let pre_handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(request.size_hint().exact(), Some(13));
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&post_redirect_url.to_string()).unwrap(),
            );
            *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
        };
    let (pre_server, pre_url) = make_server(pre_handler);

    let content = "Body on POST!";
    let request_body = create_request_body_with_content(content.to_string());

    let request = RequestBuilder::new(None, pre_url.clone(), Referrer::NoReferrer)
        .body(Some(request_body))
        .method(Method::POST)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = pre_server.close();
    let _ = post_server.close();

    assert!(response.to_actual().status.code().is_success());
}

#[test]
fn test_load_doesnt_add_host_to_hsts_list_when_url_is_http_even_if_hsts_headers_are_present() {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response
                .headers_mut()
                .typed_insert(StrictTransportSecurity::excluding_subdomains(
                    Duration::from_secs(31536000),
                ));
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let mut context = new_fetch_context(None, None);
    let response = fetch_with_context(request, &mut context);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );
    assert_eq!(
        context
            .state
            .hsts_list
            .read()
            .is_host_secure(url.host_str().unwrap()),
        false
    );
}

#[test]
fn test_load_sets_cookies_in_the_resource_manager_when_it_get_set_cookie_header_in_response() {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_static("mozillaIs=theBest"),
            );
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let mut context = new_fetch_context(None, None);

    assert_cookie_for_domain(&context.state.cookie_jar, url.as_str(), None);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let response = fetch_with_context(request, &mut context);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );

    assert_cookie_for_domain(
        &context.state.cookie_jar,
        url.as_str(),
        Some("mozillaIs=theBest"),
    );
}

#[test]
fn test_load_sets_requests_cookies_header_for_url_by_getting_cookies_from_the_resource_manager() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(
                request.headers().get(header::COOKIE).unwrap().as_bytes(),
                b"mozillaIs=theBest"
            );
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let mut context = new_fetch_context(None, None);

    {
        let mut cookie_jar = context.state.cookie_jar.write();
        let cookie = ServoCookie::new_wrapped(
            CookiePair::new("mozillaIs".to_owned(), "theBest".to_owned()),
            &url,
            CookieSource::HTTP,
        )
        .unwrap();
        cookie_jar.push(cookie, &url, CookieSource::HTTP);
    }

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let response = fetch_with_context(request, &mut context);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );
}

#[test]
fn test_load_sends_cookie_if_nonhttp() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(
                request.headers().get(header::COOKIE).unwrap().as_bytes(),
                b"mozillaIs=theBest"
            );
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let mut context = new_fetch_context(None, None);

    {
        let mut cookie_jar = context.state.cookie_jar.write();
        let cookie = ServoCookie::new_wrapped(
            CookiePair::new("mozillaIs".to_owned(), "theBest".to_owned()),
            &url,
            CookieSource::NonHTTP,
        )
        .unwrap();
        cookie_jar.push(cookie, &url, CookieSource::HTTP);
    }

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let response = fetch_with_context(request, &mut context);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );
}

#[test]
fn test_cookie_set_with_httponly_should_not_be_available_using_getcookiesforurl() {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_static("mozillaIs=theBest; HttpOnly"),
            );
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let mut context = new_fetch_context(None, None);

    assert_cookie_for_domain(&context.state.cookie_jar, url.as_str(), None);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let response = fetch_with_context(request, &mut context);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );

    assert_cookie_for_domain(
        &context.state.cookie_jar,
        url.as_str(),
        Some("mozillaIs=theBest"),
    );
    let mut cookie_jar = context.state.cookie_jar.write();
    assert!(
        cookie_jar
            .cookies_for_url(&url, CookieSource::NonHTTP)
            .is_none()
    );
}

#[test]
fn test_when_cookie_received_marked_secure_is_ignored_for_http() {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_static("mozillaIs=theBest; Secure"),
            );
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let mut context = new_fetch_context(None, None);

    assert_cookie_for_domain(&context.state.cookie_jar, url.as_str(), None);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let response = fetch_with_context(request, &mut context);

    let _ = server.close();

    assert!(response.actual_response().status.code().is_success());

    assert_cookie_for_domain(&context.state.cookie_jar, url.as_str(), None);
}

#[test]
fn test_load_sets_content_length_to_length_of_request_body() {
    let content = "This is a request body";
    let content_bytes = content.as_bytes();
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            let content_length = ContentLength(content_bytes.len() as u64);
            assert_eq!(
                request.headers().typed_get::<ContentLength>(),
                Some(content_length)
            );
            *response.body_mut() = make_body(content_bytes.to_vec());
        };
    let (server, url) = make_server(handler);

    let request_body = create_request_body_with_content(content.to_string());

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::POST)
        .body(Some(request_body))
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );
}

#[test]
fn test_load_uses_explicit_accept_from_headers_in_load_data() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(
                request
                    .headers()
                    .get(header::ACCEPT)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "text/html"
            );
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let mut accept_headers = HeaderMap::new();
    accept_headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .headers(accept_headers)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );
}

#[test]
fn test_load_sets_default_accept_to_html_xhtml_xml_and_then_anything_else() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(
                request
                    .headers()
                    .get(header::ACCEPT)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
            );
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );
}

#[test]
fn test_load_uses_explicit_accept_encoding_from_load_data_headers() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(
                request
                    .headers()
                    .get(header::ACCEPT_ENCODING)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "chunked"
            );
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let mut accept_encoding_headers = HeaderMap::new();
    accept_encoding_headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("chunked"));
    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .headers(accept_encoding_headers)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );
}

#[test]
fn test_load_sets_default_accept_encoding_to_gzip_and_deflate() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(
                request
                    .headers()
                    .get(header::ACCEPT_ENCODING)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "gzip, deflate, br, zstd"
            );
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );
}

#[test]
fn test_load_errors_when_there_a_redirect_loop() {
    let url_b_for_a = Arc::new(Mutex::new(None::<ServoUrl>));
    let url_b_for_a_clone = url_b_for_a.clone();
    let handler_a =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&url_b_for_a_clone.lock().as_ref().unwrap().to_string())
                    .unwrap(),
            );
            *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
        };
    let (server_a, url_a) = make_server(handler_a);

    let url_a_for_b = url_a.clone();
    let handler_b =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&url_a_for_b.to_string()).unwrap(),
            );
            *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
        };
    let (server_b, url_b) = make_server(handler_b);

    *url_b_for_a.lock() = Some(url_b.url());

    let request = RequestBuilder::new(None, url_a.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = server_a.close();
    let _ = server_b.close();

    assert_eq!(
        response.get_network_error(),
        Some(&NetworkError::TooManyRedirects)
    );
}

#[test]
fn test_load_succeeds_with_a_redirect_loop() {
    let url_b_for_a = Arc::new(Mutex::new(None::<ServoUrl>));
    let url_b_for_a_clone = url_b_for_a.clone();
    let handled_a = AtomicBool::new(false);
    let handler_a =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            if !handled_a.swap(true, Ordering::SeqCst) {
                response.headers_mut().insert(
                    header::LOCATION,
                    HeaderValue::from_str(&url_b_for_a_clone.lock().as_ref().unwrap().to_string())
                        .unwrap(),
                );
                *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
            } else {
                *response.body_mut() = make_body(b"Success".to_vec());
            }
        };
    let (server_a, url_a) = make_server(handler_a);

    let url_a_for_b = url_a.clone();
    let handler_b =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&url_a_for_b.to_string()).unwrap(),
            );
            *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
        };
    let (server_b, url_b) = make_server(handler_b);

    *url_b_for_a.lock() = Some(url_b.url());

    let request = RequestBuilder::new(None, url_a.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = server_a.close();
    let _ = server_b.close();

    let response = response.to_actual();
    assert_eq!(response.url_list, [url_a.url(), url_b.url(), url_a.url()]);
    assert_eq!(
        *response.body.lock(),
        ResponseBody::Done(b"Success".to_vec())
    );
}

#[test]
fn test_load_follows_a_redirect() {
    let post_handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(request.method(), Method::GET);
            *response.body_mut() = make_body(b"Yay!".to_vec());
        };
    let (post_server, post_url) = make_server(post_handler);

    let post_redirect_url = post_url.clone();
    let pre_handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert_eq!(request.method(), Method::GET);
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&post_redirect_url.to_string()).unwrap(),
            );
            *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
        };
    let (pre_server, pre_url) = make_server(pre_handler);

    let request = RequestBuilder::new(None, pre_url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = pre_server.close();
    let _ = post_server.close();

    let internal_response = response.internal_response.unwrap();
    assert!(internal_response.status.clone().code().is_success());
    assert_eq!(
        *internal_response.body.lock(),
        ResponseBody::Done(b"Yay!".to_vec())
    );
}

#[test]
fn test_redirect_from_x_to_y_provides_y_cookies_from_y() {
    let shared_url_y = Arc::new(Mutex::new(None::<ServoUrl>));
    let shared_url_y_clone = shared_url_y.clone();
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            let path = request.uri().path();
            if path == "/com/" {
                assert_eq!(
                    request.headers().get(header::COOKIE).unwrap().as_bytes(),
                    b"mozillaIsNot=dotOrg"
                );
                let location = shared_url_y.lock().as_ref().unwrap().to_string();
                response.headers_mut().insert(
                    header::LOCATION,
                    HeaderValue::from_str(&location.to_string()).unwrap(),
                );
                *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
            } else if path == "/org/" {
                assert_eq!(
                    request.headers().get(header::COOKIE).unwrap().as_bytes(),
                    b"mozillaIs=theBest"
                );
                *response.body_mut() = make_body(b"Yay!".to_vec());
            } else {
                panic!("unexpected path {:?}", path)
            }
        };
    let (server, url) = make_server(handler);
    let port = url.port().unwrap();

    assert_eq!(url.host_str(), Some("localhost"));
    let ip = "127.0.0.1".parse().unwrap();
    let mut host_table = HashMap::new();
    host_table.insert("mozilla.com".to_owned(), ip);
    host_table.insert("mozilla.org".to_owned(), ip);

    replace_host_table(host_table);

    let url_x = ServoUrl::parse(&format!("http://mozilla.com:{}/com/", port)).unwrap();
    let url_y = ServoUrl::parse(&format!("http://mozilla.org:{}/org/", port)).unwrap();
    *shared_url_y_clone.lock() = Some(url_y.clone());

    let mut context = new_fetch_context(None, None);
    {
        let mut cookie_jar = context.state.cookie_jar.write();
        let cookie_x = ServoCookie::new_wrapped(
            CookiePair::new("mozillaIsNot".to_owned(), "dotOrg".to_owned()),
            &url_x,
            CookieSource::HTTP,
        )
        .unwrap();

        cookie_jar.push(cookie_x, &url_x, CookieSource::HTTP);

        let cookie_y = ServoCookie::new_wrapped(
            CookiePair::new("mozillaIs".to_owned(), "theBest".to_owned()),
            &url_y,
            CookieSource::HTTP,
        )
        .unwrap();
        cookie_jar.push(cookie_y, &url_y, CookieSource::HTTP);
    }

    let request = RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url_x.clone(), None),
        Referrer::NoReferrer,
    )
    .method(Method::GET)
    .destination(Destination::Document)
    .origin(mock_origin())
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .credentials_mode(CredentialsMode::Include)
    .policy_container(Default::default())
    .build();

    let response = fetch_with_context(request, &mut context);

    let _ = server.close();

    let internal_response = response.internal_response.unwrap();
    assert!(internal_response.status.clone().code().is_success());
    assert_eq!(
        *internal_response.body.lock(),
        ResponseBody::Done(b"Yay!".to_vec())
    );
}

#[test]
fn test_redirect_from_x_to_x_provides_x_with_cookie_from_first_response() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            let path = request.uri().path();
            if path == "/initial/" {
                response.headers_mut().insert(
                    header::SET_COOKIE,
                    HeaderValue::from_static("mozillaIs=theBest; path=/;"),
                );
                let location = "/subsequent/".to_string();
                response.headers_mut().insert(
                    header::LOCATION,
                    HeaderValue::from_str(&location.to_string()).unwrap(),
                );
                *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
            } else if path == "/subsequent/" {
                assert_eq!(
                    request.headers().get(header::COOKIE).unwrap().as_bytes(),
                    b"mozillaIs=theBest"
                );
                *response.body_mut() = make_body(b"Yay!".to_vec());
            } else {
                panic!("unexpected path {:?}", path)
            }
        };
    let (server, url) = make_server(handler);

    let url = url.join("/initial/").unwrap();

    let request = RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url.clone(), None),
        Referrer::NoReferrer,
    )
    .method(Method::GET)
    .destination(Destination::Document)
    .origin(mock_origin())
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .credentials_mode(CredentialsMode::Include)
    .policy_container(Default::default())
    .build();

    let response = fetch(request, None);

    let _ = server.close();

    let internal_response = response.internal_response.unwrap();
    assert!(internal_response.status.clone().code().is_success());
    assert_eq!(
        *internal_response.body.lock(),
        ResponseBody::Done(b"Yay!".to_vec())
    );
}

#[test]
fn test_if_auth_creds_not_in_url_but_in_cache_it_sets_it() {
    let handler =
        move |request: HyperRequest<Incoming>,
              _response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            let expected = Authorization::basic("username", "test");
            assert_eq!(
                request.headers().typed_get::<Authorization<Basic>>(),
                Some(expected)
            );
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .mode(RequestMode::Navigate)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let mut context = new_fetch_context(None, None);

    let auth_entry = AuthCacheEntry {
        user_name: "username".to_owned(),
        password: "test".to_owned(),
    };

    context.state.auth_cache.write().entries.insert(
        url.origin().clone().ascii_serialization().into_owned(),
        auth_entry,
    );

    let response = fetch_with_context(request, &mut context);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );
}

#[test]
fn test_auth_ui_needs_www_auth() {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            *response.status_mut() = StatusCode::UNAUTHORIZED;
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .mode(RequestMode::Navigate)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let response = fetch(request, None);

    let _ = server.close();

    assert_eq!(
        response.internal_response.unwrap().status,
        StatusCode::UNAUTHORIZED
    );
}

#[test]
fn test_determine_requests_referrer_shorter_than_4k() {
    let url_str = "http://username:password@example.com/such/short/referer?query#fragment";
    let referrer_source = ServoUrl::parse(url_str).unwrap();
    let current_url = ServoUrl::parse("http://example.com/current/url").unwrap();
    let referrer_policy = ReferrerPolicy::UnsafeUrl;

    let referer = determine_requests_referrer(referrer_policy, referrer_source, current_url);

    assert_eq!(
        referer.unwrap().as_str(),
        "http://example.com/such/short/referer?query"
    );
}

#[test]
fn test_determine_requests_referrer_longer_than_4k() {
    let long_url_str = format!(
        "http://username:password@example.com/such/{}/referer?query#fragment",
        "long".repeat(1024)
    );
    let referrer_source = ServoUrl::parse(&long_url_str).unwrap();
    let current_url = ServoUrl::parse("http://example.com/current/url").unwrap();
    let referrer_policy = ReferrerPolicy::UnsafeUrl;

    let referer = determine_requests_referrer(referrer_policy, referrer_source, current_url);

    assert_eq!(referer.unwrap().as_str(), "http://example.com/");
}

#[test]
fn test_fetch_compressed_response_update_count() {
    // contents of ../../tests/wpt/tests/fetch/content-encoding/br/resources/foo.text.br
    const DATA_BROTLI_COMPRESSED: [u8; 15] = [
        0xe1, 0x18, 0x48, 0xc1, 0x2f, 0x65, 0xf6, 0x16, 0x9f, 0x05, 0x01, 0xbb, 0x20, 0x00, 0x06,
    ];
    const DATA_DECOMPRESSED_LEN: usize = 10500;

    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response
                .headers_mut()
                .insert(header::CONTENT_ENCODING, HeaderValue::from_static("br"));
            *response.body_mut() = make_body(DATA_BROTLI_COMPRESSED.to_vec());
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    struct FetchResponseCollector {
        sender: Option<tokio::sync::oneshot::Sender<usize>>,
        update_count: usize,
    }
    impl FetchTaskTarget for FetchResponseCollector {
        fn process_request_body(&mut self, _: &Request) {}
        fn process_response(&mut self, _: &Request, _: &Response) {}
        fn process_response_chunk(&mut self, _: &Request, _: bytes::Bytes) {
            self.update_count += 1;
        }
        /// Fired when the response is fully fetched
        fn process_response_eof(&mut self, _: &Request, _: &Response) {
            let _ = self.sender.take().unwrap().send(self.update_count);
        }
        fn process_csp_violations(&mut self, _: &Request, _: Vec<csp::Violation>) {}

        fn process_response_length_hint(&mut self, _: &Request, _: usize) {}
    }

    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut target = FetchResponseCollector {
        sender: Some(sender),
        update_count: 0,
    };
    let response_update_count = spawn_blocking_task(async move {
        methods::fetch(request, &mut target, &mut new_fetch_context(None, None)).await;
        receiver.await.unwrap()
    });

    server.close();

    const EXPECTED_UPDATE_COUNT: usize =
        (DATA_DECOMPRESSED_LEN + DECODER_BUFFER_SIZE - 1) / DECODER_BUFFER_SIZE;
    assert_eq!(response_update_count, EXPECTED_UPDATE_COUNT);
}

#[test]
fn test_origin_serialization_compatibility() {
    let ensure_serialiations_match = |url_string| {
        let url = Url::parse(url_string).unwrap();
        let origin = ImmutableOrigin::new(&url);
        let serialized = format!("{}", serialize_origin(&origin));
        assert_eq!(serialized, origin.ascii_serialization().as_ref());
    };

    ensure_serialiations_match("https://example.com");
    ensure_serialiations_match("https://example.com:443");

    ensure_serialiations_match("http://example.com");
    ensure_serialiations_match("http://example.com:80");

    ensure_serialiations_match("https://example.com:1234");
    ensure_serialiations_match("http://example.com:1234");

    ensure_serialiations_match("data:,dataurltexta");
}

#[test]
fn test_user_credentials_prompt_when_proxy_authentication_is_required() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            let expected = Authorization::basic("username", "test");
            if let Some(credentials) = request.headers().typed_get::<Authorization<Basic>>() {
                if credentials == expected {
                    *response.status_mut() = StatusCode::OK;
                } else {
                    *response.status_mut() = StatusCode::UNAUTHORIZED;
                }
            } else {
                *response.status_mut() = StatusCode::PROXY_AUTHENTICATION_REQUIRED;
            }
        };
    let (server, url) = make_server(handler);

    let mut request = RequestBuilder::new(Some(TEST_WEBVIEW_ID), url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .mode(RequestMode::Navigate)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    request.traversable_for_user_prompts =
        TraversableForUserPrompts::TraversableNavigable(Default::default());

    let (embedder_proxy, embedder_receiver) = create_generic_embedder_proxy_and_receiver();
    let _ = receive_credential_prompt_msgs(
        embedder_receiver,
        Some(AuthenticationResponse {
            username: "username".into(),
            password: "test".into(),
        }),
    );

    let mut context = new_fetch_context(None, Some(embedder_proxy));

    let response = fetch_with_context(request, &mut context);

    let _ = server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_success()
    );
}

#[test]
fn test_prompt_credentials_when_client_receives_unauthorized_response() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            let expected = Authorization::basic("username", "test");
            if let Some(credentials) = request.headers().typed_get::<Authorization<Basic>>() {
                if credentials == expected {
                    *response.status_mut() = StatusCode::OK;
                } else {
                    *response.status_mut() = StatusCode::UNAUTHORIZED;
                }
            } else {
                *response.status_mut() = StatusCode::UNAUTHORIZED;
            }

            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Basic"));
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(Some(TEST_WEBVIEW_ID), url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .mode(RequestMode::Navigate)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let (embedder_proxy, embedder_receiver) = create_generic_embedder_proxy_and_receiver();
    let _ = receive_credential_prompt_msgs(
        embedder_receiver,
        Some(AuthenticationResponse {
            username: "username".into(),
            password: "test".into(),
        }),
    );
    let mut context = new_fetch_context(None, Some(embedder_proxy));

    let response = fetch_with_context(request, &mut context);

    server.close();

    assert_eq!(
        response.internal_response.unwrap().status.code(),
        StatusCode::OK
    );
}

#[test]
fn test_dont_prompt_credentials_when_unauthorized_response_contains_no_www_authenticate_header() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            assert!(
                request
                    .headers()
                    .typed_get::<Authorization<Basic>>()
                    .is_none()
            );
            *response.status_mut() = StatusCode::UNAUTHORIZED;
        };

    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(Some(TEST_WEBVIEW_ID), url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .mode(RequestMode::Navigate)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let (embedder_proxy, embedder_receiver) = create_generic_embedder_proxy_and_receiver();
    let handle = std::thread::spawn(move || {
        loop {
            let Ok(msg) = embedder_receiver.recv() else {
                return;
            };
            match msg {
                net::embedder::NetToEmbedderMsg::RequestAuthentication(..) => {
                    panic!(
                        "Should not have requested authentication as there's no www-authenticate header"
                    );
                },
                net::embedder::NetToEmbedderMsg::WebResourceRequested(..) => {},
                _ => unreachable!(),
            }
        }
    });
    let mut context = new_fetch_context(None, Some(embedder_proxy));

    let response = fetch_with_context(request, &mut context);

    server.close();

    assert_eq!(
        response.internal_response.unwrap().status.code(),
        StatusCode::UNAUTHORIZED
    );

    // Without this join we won't notice if the mock embedder thread panics!
    drop(context); // Dropping this context causes the embedder thread to exit.
    handle.join().unwrap();
}

#[test]
fn test_prompt_credentials_user_cancels_dialog_input() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            let expected = Authorization::basic("username", "test");
            if let Some(credentials) = request.headers().typed_get::<Authorization<Basic>>() {
                if credentials == expected {
                    *response.status_mut() = StatusCode::OK;
                } else {
                    *response.status_mut() = StatusCode::UNAUTHORIZED;
                }
            } else {
                *response.status_mut() = StatusCode::UNAUTHORIZED;
            }
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(Some(TEST_WEBVIEW_ID), url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .mode(RequestMode::Navigate)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let (embedder_proxy, embedder_receiver) = create_generic_embedder_proxy_and_receiver();
    let _ = receive_credential_prompt_msgs(embedder_receiver, None);
    let mut context = new_fetch_context(None, Some(embedder_proxy));

    let response = fetch_with_context(request, &mut context);

    server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_client_error()
    );
}

#[test]
fn test_prompt_credentials_user_input_incorrect_credentials() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            let expected = Authorization::basic("username", "test");
            if let Some(credentials) = request.headers().typed_get::<Authorization<Basic>>() {
                if credentials == expected {
                    *response.status_mut() = StatusCode::OK;
                } else {
                    *response.status_mut() = StatusCode::UNAUTHORIZED;
                }
            } else {
                *response.status_mut() = StatusCode::UNAUTHORIZED;
            }
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(Some(TEST_WEBVIEW_ID), url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .mode(RequestMode::Navigate)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let (embedder_proxy, embedder_receiver) = create_generic_embedder_proxy_and_receiver();
    let _ = receive_credential_prompt_msgs(
        embedder_receiver,
        Some(AuthenticationResponse {
            username: "test".into(),
            password: "test".into(),
        }),
    );
    let mut context = new_fetch_context(None, Some(embedder_proxy));

    let response = fetch_with_context(request, &mut context);

    server.close();

    assert!(
        response
            .internal_response
            .unwrap()
            .status
            .code()
            .is_client_error()
    );
}

#[test]
fn test_prompt_credentials_user_input_incorrect_mode() {
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            let expected = Authorization::basic("test", "test");
            if let Some(credentials) = request.headers().typed_get::<Authorization<Basic>>() {
                if credentials == expected {
                    *response.status_mut() = StatusCode::OK;
                } else {
                    *response.status_mut() = StatusCode::UNAUTHORIZED;
                }
            } else {
                *response.status_mut() = StatusCode::UNAUTHORIZED;
            }
        };
    let (server, url) = make_server(handler);

    let request = RequestBuilder::new(Some(TEST_WEBVIEW_ID), url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .mode(RequestMode::SameOrigin)
        .origin(mock_origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .credentials_mode(CredentialsMode::Include)
        .policy_container(Default::default())
        .build();

    let (embedder_proxy, embedder_receiver) = create_generic_embedder_proxy_and_receiver();
    let _ = receive_credential_prompt_msgs(
        embedder_receiver,
        Some(AuthenticationResponse {
            username: "test".into(),
            password: "test".into(),
        }),
    );
    let mut context = new_fetch_context(None, Some(embedder_proxy));

    let response = fetch_with_context(request, &mut context);

    server.close();

    assert!(response.internal_response.is_none());
}

/// Collects all network events from the devtools channel.
/// Drains all available events after the fetch has completed.
fn collect_all_network_events(devtools_port: &Receiver<DevtoolsControlMsg>) -> Vec<NetworkEvent> {
    let mut events = Vec::new();

    // Drain all available events from the channel.
    // Since fetch has already completed, all events should be queued.
    while let Ok(msg) = devtools_port.try_recv() {
        if let DevtoolsControlMsg::FromChrome(ChromeToDevtoolsControlMsg::NetworkEvent(
            _,
            net_event,
        )) = msg
        {
            events.push(net_event);
        }
    }
    events
}

#[test]
fn test_security_info_for_https_connection() {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            *response.body_mut() = make_body(b"Hello HTTPS!".to_vec());
        };

    let (server, mut url) = make_ssl_server(handler);
    url.as_mut_url().set_scheme("https").unwrap();

    let (devtools_sender, devtools_receiver) = unbounded();

    let mut context = new_fetch_context(Some(devtools_sender), None);

    // The server certificate is self-signed, so we need to add an override
    // so that the connection works properly.
    for certificate in server.certificates.as_ref().unwrap().iter() {
        context.state.override_manager.add_override(certificate);
    }

    let request = RequestBuilder::new(Some(TEST_WEBVIEW_ID), url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .origin(url.clone().origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let response = fetch_with_context(request, &mut context);
    server.close();

    assert!(response.status.code().is_success());

    let events = collect_all_network_events(&devtools_receiver);
    let security_info_event = events.iter().find_map(|event| {
        if let NetworkEvent::SecurityInfo(info) = event {
            Some(info)
        } else {
            None
        }
    });

    assert!(
        security_info_event.is_some(),
        "Expected to receive a SecurityInfo event for HTTPS connection"
    );

    let security_info = security_info_event.unwrap();
    assert!(
        security_info.security_info.is_some(),
        "Expected security_info to contain TLS details for HTTPS connection"
    );

    let tls_info = security_info.security_info.as_ref().unwrap();
    assert!(
        tls_info.protocol_version.is_some(),
        "Expected protocol_version to be set for HTTPS connection"
    );

    let protocol = tls_info.protocol_version.as_ref().unwrap();
    assert!(
        format!("{:?}", protocol).starts_with("TLS"),
        "Expected TLS protocol version, got: {:?}",
        protocol
    );

    assert!(
        tls_info.cipher_suite.is_some(),
        "Expected cipher_suite to be set for HTTPS connection"
    );
}

#[test]
fn test_no_security_info_for_http_connection() {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            *response.body_mut() = make_body(b"Hello HTTP!".to_vec());
        };

    let (server, url) = make_server(handler);

    let (devtools_sender, devtools_receiver) = unbounded();

    let request = RequestBuilder::new(Some(TEST_WEBVIEW_ID), url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .body(None)
        .destination(Destination::Document)
        .origin(url.clone().origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();

    let mut context = new_fetch_context(Some(devtools_sender), None);

    let response = fetch_with_context(request, &mut context);
    server.close();

    assert!(response.status.code().is_success());

    let events = collect_all_network_events(&devtools_receiver);
    let security_info_events: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let NetworkEvent::SecurityInfo(info) = event {
                Some(info)
            } else {
                None
            }
        })
        .collect();

    for security_info in &security_info_events {
        assert!(
            security_info.security_info.is_none(),
            "HTTP connection should not have TLS security info"
        );
    }
}

#[test]
fn test_stale_while_revalidate_serves_cached_and_revalidates_in_background() {
    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count_clone = request_count.clone();
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            request_count_clone.fetch_add(1, Ordering::SeqCst);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("max-age=0, stale-while-revalidate=30"),
            );
            *response.body_mut() = make_body(b"content".to_vec());
        };
    let (server, url) = make_server(handler);

    let mut context = new_fetch_context(None, None);

    let build_request = || {
        RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
            .method(Method::GET)
            .destination(Destination::Document)
            .origin(url.clone().origin())
            .pipeline_id(Some(TEST_PIPELINE_ID))
            .policy_container(Default::default())
            .build()
    };

    let response = fetch_with_context(build_request(), &mut context);
    assert!(response.actual_response().status.code().is_success());
    assert_eq!(request_count.load(Ordering::SeqCst), 1);

    // the stored response is stale but within the stale-while-revalidate window, so
    // it is served from cache immediately and a background revalidation is spawned.
    let response = fetch_with_context(build_request(), &mut context);
    assert!(response.actual_response().status.code().is_success());

    let deadline = Instant::now() + Duration::from_secs(5);
    while request_count.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        request_count.load(Ordering::SeqCst),
        2,
        "exactly one background revalidation should have hit the server"
    );

    let _ = server.close();
}

/// Collects the response and the number of body bytes that a fetch delivers.
struct BodyLengthCollector {
    sender: Option<tokio::sync::oneshot::Sender<(Response, usize)>>,
    length: usize,
}

impl FetchTaskTarget for BodyLengthCollector {
    fn process_request_body(&mut self, _: &Request) {}
    fn process_response(&mut self, _: &Request, _: &Response) {}
    fn process_response_chunk(&mut self, _: &Request, chunk: Bytes) {
        self.length += chunk.len();
    }
    fn process_response_eof(&mut self, _: &Request, response: &Response) {
        let _ = self
            .sender
            .take()
            .unwrap()
            .send((response.clone(), self.length));
    }
    fn process_csp_violations(&mut self, _: &Request, _: Vec<csp::Violation>) {}
    fn process_response_length_hint(&mut self, _: &Request, _: usize) {}
}

/// Fetch `url` the way a video element starts a load, and return the response and the number of
/// body bytes delivered.
fn fetch_video(url: &UrlWithBlobClaim, context: &mut FetchContext) -> (Response, usize) {
    let mut headers = HeaderMap::new();
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=0-"));
    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .destination(Destination::Video)
        .headers(headers)
        .origin(url.clone().origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut target = BodyLengthCollector {
        sender: Some(sender),
        length: 0,
    };
    spawn_blocking_task(async move {
        methods::fetch(request, &mut target, context).await;
        receiver.await.unwrap()
    })
}

#[test]
fn test_large_media_response_is_neither_kept_nor_stored() {
    const LENGTH: usize = 8 * 1024 * 1024 + 1;
    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count_clone = request_count.clone();
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            request_count_clone.fetch_add(1, Ordering::SeqCst);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("max-age=3600"),
            );
            *response.body_mut() = make_body(vec![0; LENGTH]);
        };
    let (server, url) = make_server(handler);
    let mut context = new_fetch_context(None, None);

    for expected_request_count in 1..=2 {
        let (response, body_length) = fetch_video(&url, &mut context);
        assert_eq!(body_length, LENGTH);
        assert!(matches!(
            &*response.actual_response().body.lock(),
            ResponseBody::Done(body) if body.is_empty()
        ));
        assert_eq!(request_count.load(Ordering::SeqCst), expected_request_count);
    }

    let _ = server.close();
}

#[test]
fn test_small_media_response_is_stored() {
    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count_clone = request_count.clone();
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            request_count_clone.fetch_add(1, Ordering::SeqCst);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("max-age=3600"),
            );
            *response.body_mut() = make_body(b"video".to_vec());
        };
    let (server, url) = make_server(handler);
    let mut context = new_fetch_context(None, None);

    let (response, body_length) = fetch_video(&url, &mut context);
    assert_eq!(body_length, 5);
    assert!(matches!(response.cache_state, CacheState::None));

    let (response, body_length) = fetch_video(&url, &mut context);
    assert_eq!(body_length, 5);
    assert!(matches!(response.cache_state, CacheState::Local));
    assert_eq!(request_count.load(Ordering::SeqCst), 1);

    let _ = server.close();
}

fn build_cacheable_request(url: &UrlWithBlobClaim) -> Request {
    RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .method(Method::GET)
        .destination(Destination::Document)
        .origin(url.clone().origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
        .build()
}

fn stored_response_count(context: &FetchContext, request: &Request) -> usize {
    spawn_blocking_task(context.state.http_cache.stored_response_count(request))
}

#[test]
fn test_truncated_response_is_not_served_from_cache() {
    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count_clone = request_count.clone();
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            request_count_clone.fetch_add(1, Ordering::SeqCst);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("max-age=3600"),
            );
            // Send part of the body, let the server flush it, then end the body before its
            // length, which makes the server close the connection.
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from_static("100"));
            let mut polls = 0;
            let chunks = stream::poll_fn(move |context| {
                polls += 1;
                match polls {
                    1 => Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"truncated"))))),
                    2 => {
                        context.waker().wake_by_ref();
                        Poll::Pending
                    },
                    _ => Poll::Ready(None),
                }
            });
            *response.body_mut() = StreamBody::new(chunks).boxed();
        };
    let (server, url) = make_server(handler);
    let mut context = new_fetch_context(None, None);

    let response = fetch_with_context(build_cacheable_request(&url), &mut context);
    assert_eq!(
        *response.actual_response().body.lock(),
        ResponseBody::Done(b"truncated".to_vec())
    );
    assert_eq!(
        stored_response_count(&context, &build_cacheable_request(&url)),
        0
    );

    let response = fetch_with_context(build_cacheable_request(&url), &mut context);
    assert!(matches!(response.cache_state, CacheState::None));
    assert_eq!(request_count.load(Ordering::SeqCst), 2);

    let _ = server.close();
}

#[test]
fn test_cancelled_response_is_removed_from_cache() {
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("max-age=3600"),
            );
            let chunks = stream::repeat_with(|| Ok(Frame::data(Bytes::from_static(&[0; 1024]))));
            *response.body_mut() = StreamBody::new(chunks).boxed();
        };
    let (server, url) = make_server(handler);
    let context = new_fetch_context(None, None);

    /// Cancels the fetch once the response headers arrive.
    struct CancellingTarget {
        cancellation_listener: Arc<CancellationListener>,
        sender: Option<tokio::sync::oneshot::Sender<()>>,
    }
    impl FetchTaskTarget for CancellingTarget {
        fn process_request_body(&mut self, _: &Request) {}
        fn process_response(&mut self, _: &Request, _: &Response) {
            self.cancellation_listener.cancel();
        }
        fn process_response_chunk(&mut self, _: &Request, _: Bytes) {}
        fn process_response_eof(&mut self, _: &Request, _: &Response) {
            let _ = self.sender.take().unwrap().send(());
        }
        fn process_csp_violations(&mut self, _: &Request, _: Vec<csp::Violation>) {}
        fn process_response_length_hint(&mut self, _: &Request, _: usize) {}
    }

    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut target = CancellingTarget {
        cancellation_listener: context.cancellation_listener.clone(),
        sender: Some(sender),
    };
    let request = build_cacheable_request(&url);
    spawn_blocking_task(async {
        methods::fetch(request, &mut target, &context).await;
        receiver.await.unwrap()
    });

    assert_eq!(
        stored_response_count(&context, &build_cacheable_request(&url)),
        0
    );
    assert_eq!(context.state.http_cache.len(), 0);

    let _ = server.close();
}

/// Start `request` with `context` in the background, and return a receiver for its response and
/// the number of body bytes it delivers.
fn start_fetch(
    request: Request,
    context: FetchContext,
) -> tokio::sync::oneshot::Receiver<(Response, usize)> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut target = BodyLengthCollector {
        sender: Some(sender),
        length: 0,
    };
    spawn_task(async move {
        methods::fetch(request, &mut target, &context).await;
    });
    receiver
}

/// Wait until `condition` holds, for at most ten seconds.
fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wait for the end of a fetch started with `start_fetch`, for at most ten seconds.
fn wait_for_fetch<T>(mut receiver: tokio::sync::oneshot::Receiver<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(result) = receiver.try_recv() {
            return result;
        }
        assert!(Instant::now() < deadline, "the fetch did not end");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn awaiting_consumer_count(context: &FetchContext, request: &Request) -> usize {
    spawn_blocking_task(context.state.http_cache.awaiting_consumer_count(request))
}

/// A cacheable server whose first response sends `comp`, then `lete` once the returned sender
/// fires; later responses send `complete` at once.
fn make_held_server() -> (
    net::test_util::Server,
    UrlWithBlobClaim,
    tokio::sync::oneshot::Sender<()>,
    Arc<AtomicUsize>,
) {
    let (release_sender, release) = tokio::sync::oneshot::channel::<()>();
    let release = Mutex::new(Some(release));
    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count_clone = request_count.clone();
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            request_count_clone.fetch_add(1, Ordering::SeqCst);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("max-age=3600"),
            );
            *response.body_mut() = match release.lock().take() {
                Some(release) => {
                    let first = stream::iter([Ok(Frame::data(Bytes::from_static(b"comp")))]);
                    let last = stream::once(async move {
                        let _ = release.await;
                        Ok(Frame::data(Bytes::from_static(b"lete")))
                    });
                    StreamBody::new(futures::StreamExt::chain(first, last)).boxed()
                },
                None => make_body(b"complete".to_vec()),
            };
        };
    let (server, url) = make_server(handler);
    (server, url, release_sender, request_count)
}

#[test]
fn test_fetch_waiting_for_an_aborted_response_uses_the_network() {
    let (server, url, release, request_count) = make_held_server();
    let context = new_fetch_context(None, None);
    let mut waiting_context = context.clone();
    waiting_context.cancellation_listener = Arc::new(Default::default());
    let request = build_cacheable_request(&url);

    let _first_fetch = start_fetch(build_cacheable_request(&url), context.clone());
    wait_until(|| stored_response_count(&context, &request) == 1);
    let waiting_fetch = start_fetch(build_cacheable_request(&url), waiting_context);
    wait_until(|| awaiting_consumer_count(&context, &request) == 1);
    context.cancellation_listener.cancel();
    let _ = release.send(());

    let (response, body_length) = wait_for_fetch(waiting_fetch);
    assert_eq!(body_length, b"complete".len());
    assert!(matches!(response.cache_state, CacheState::None));
    assert_eq!(request_count.load(Ordering::SeqCst), 2);

    let _ = server.close();
}

#[test]
fn test_fetch_waiting_for_a_response_whose_fetch_failed_is_woken_up() {
    let (server, url, release, request_count) = make_held_server();
    let context = new_fetch_context(None, None);
    let request = build_cacheable_request(&url);
    let mut request_with_wrong_integrity = build_cacheable_request(&url);
    request_with_wrong_integrity.integrity_metadata =
        "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_owned();

    let failing_fetch = start_fetch(request_with_wrong_integrity, context.clone());
    wait_until(|| stored_response_count(&context, &request) == 1);
    let waiting_fetch = start_fetch(build_cacheable_request(&url), context.clone());
    wait_until(|| awaiting_consumer_count(&context, &request) == 1);
    let _ = release.send(());

    let (response, _) = wait_for_fetch(failing_fetch);
    assert!(response.is_network_error());
    let (response, body_length) = wait_for_fetch(waiting_fetch);
    assert_eq!(body_length, b"complete".len());
    assert!(matches!(response.cache_state, CacheState::Local));
    assert_eq!(request_count.load(Ordering::SeqCst), 1);

    let _ = server.close();
}

const STREAM_CHUNK_SIZE: usize = 64 * 1024;
const STREAM_LENGTH: usize = 64 * 1024 * 1024;

/// Chunk `index` of the bodies that the streaming servers send. Every chunk differs, so a lost,
/// duplicated or reordered chunk does not compare equal.
fn stream_chunk(index: usize) -> Bytes {
    let mut chunk = vec![index as u8; STREAM_CHUNK_SIZE];
    chunk[..8].copy_from_slice(&(index as u64).to_le_bytes());
    Bytes::from(chunk)
}

/// A body of `length` bytes: the first chunk, then the others once `release` fires. `sent`
/// counts the bytes handed to the connection.
fn streaming_body(
    length: usize,
    release: tokio::sync::oneshot::Receiver<()>,
    sent: Arc<AtomicUsize>,
) -> impl futures::Stream<Item = Result<Frame<Bytes>, hyper::Error>> {
    use futures::StreamExt;
    let rest = stream::once(release).flat_map(move |_| stream::iter(1..length / STREAM_CHUNK_SIZE));
    stream::iter([0]).chain(rest).map(move |index| {
        sent.fetch_add(STREAM_CHUNK_SIZE, Ordering::SeqCst);
        Ok(Frame::data(stream_chunk(index)))
    })
}

struct StreamingServer {
    server: net::test_util::Server,
    url: UrlWithBlobClaim,
    /// For each request, in the order they arrive: lets the server send the body after its first
    /// chunk.
    releases: Vec<Option<tokio::sync::oneshot::Sender<()>>>,
    /// For each request: the number of body bytes the server has handed to its connection.
    sent: Vec<Arc<AtomicUsize>>,
    /// Whether the last request came over HTTP/2.
    used_http2: Arc<AtomicBool>,
}

impl StreamingServer {
    fn release(&mut self, request: usize) {
        let _ = self.releases[request].take().unwrap().send(());
    }

    fn sent(&self, request: usize) -> usize {
        self.sent[request].load(Ordering::SeqCst)
    }
}

/// A server for `requests` requests to one URL, which answers each with a body of `length` bytes.
/// It sends the first chunk, then waits for `StreamingServer::release`, then sends the rest as
/// fast as the client reads it. If `cacheable`, the HTTP cache may store the responses.
fn make_streaming_server(
    http2: bool,
    requests: usize,
    length: usize,
    cacheable: bool,
) -> StreamingServer {
    let (releases, release_receivers): (Vec<_>, Vec<_>) = (0..requests)
        .map(|_| {
            let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
            (Some(sender), Some(receiver))
        })
        .unzip();
    let release_receivers = Mutex::new(release_receivers);
    let sent: Vec<_> = (0..requests)
        .map(|_| Arc::new(AtomicUsize::new(0)))
        .collect();
    let sent_clone = sent.clone();
    let next_request = AtomicUsize::new(0);
    let used_http2 = Arc::new(AtomicBool::new(false));
    let used_http2_clone = used_http2.clone();
    let handler =
        move |request: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            used_http2_clone.store(request.version() == http::Version::HTTP_2, Ordering::SeqCst);
            let index = next_request.fetch_add(1, Ordering::SeqCst);
            response
                .headers_mut()
                .typed_insert(ContentLength(length as u64));
            if cacheable {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("max-age=3600"),
                );
            }
            let release = release_receivers
                .lock()
                .get_mut(index)
                .and_then(Option::take)
                .expect("more requests than expected");
            *response.body_mut() =
                StreamBody::new(streaming_body(length, release, sent_clone[index].clone())).boxed();
        };
    let (server, url) = if http2 {
        make_h2_ssl_server(handler)
    } else {
        make_server(handler)
    };
    StreamingServer {
        server,
        url,
        releases,
        sent,
        used_http2,
    }
}

/// A server for one HTTP/1.1 request, which announces `STREAM_LENGTH` bytes and sends the first
/// chunk, then waits for the returned sender, sends the next `chunks - 1` chunks and closes the
/// connection. The flag is set once the connection is closed.
fn make_closing_server(
    chunks: usize,
) -> (
    UrlWithBlobClaim,
    std::sync::mpsc::Sender<()>,
    Arc<AtomicBool>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = ServoUrl::parse(&format!(
        "http://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    ))
    .unwrap();
    let (release_sender, release) = std::sync::mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let closed_clone = closed.clone();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let mut buffer = [0; 1024];
            let length = stream.read(&mut buffer).unwrap();
            assert!(length > 0);
            request.extend_from_slice(&buffer[..length]);
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {STREAM_LENGTH}\r\n\r\n"
        )
        .unwrap();
        stream.write_all(&stream_chunk(0)).unwrap();
        release.recv().unwrap();
        for index in 1..chunks {
            stream.write_all(&stream_chunk(index)).unwrap();
        }
        stream.shutdown(std::net::Shutdown::Both).unwrap();
        closed_clone.store(true, Ordering::SeqCst);
    });
    (UrlWithBlobClaim::new(url, None), release_sender, closed)
}

fn new_streaming_fetch_context(server: &StreamingServer) -> FetchContext {
    let context = new_fetch_context(None, None);
    for certificate in server.server.certificates.iter().flatten() {
        context.state.override_manager.add_override(certificate);
    }
    context
}

/// A context for another fetch with the same HTTP state, with a listener of its own.
fn with_new_listener(context: &FetchContext) -> FetchContext {
    let mut context = context.clone();
    context.cancellation_listener = Arc::new(Default::default());
    context
}

fn build_streaming_request(url: &UrlWithBlobClaim) -> RequestBuilder {
    RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .destination(Destination::Video)
        .origin(url.clone().origin())
        .pipeline_id(Some(TEST_PIPELINE_ID))
        .policy_container(Default::default())
}

/// Compares a body with what the streaming servers send.
#[derive(Default)]
struct StreamVerifier {
    received: usize,
    mismatch: bool,
}

impl StreamVerifier {
    fn verify(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let offset = self.received % STREAM_CHUNK_SIZE;
            let length = data.len().min(STREAM_CHUNK_SIZE - offset);
            let expected = stream_chunk(self.received / STREAM_CHUNK_SIZE);
            self.mismatch |= data[..length] != expected[offset..offset + length];
            data = &data[length..];
            self.received += length;
        }
    }

    /// Whether the body arrived whole and intact.
    fn is_intact(&self, length: usize) -> bool {
        self.received == length && !self.mismatch
    }
}

/// Verifies the body of a fetch from a streaming server, and publishes how many bytes arrived
/// so far.
struct StreamCollector {
    sender: Option<tokio::sync::oneshot::Sender<(Response, StreamVerifier)>>,
    verifier: Option<StreamVerifier>,
    received: Arc<AtomicUsize>,
}

impl FetchTaskTarget for StreamCollector {
    fn process_request_body(&mut self, _: &Request) {}
    fn process_response(&mut self, _: &Request, _: &Response) {}
    fn process_response_chunk(&mut self, _: &Request, chunk: Bytes) {
        let verifier = self.verifier.as_mut().unwrap();
        verifier.verify(&chunk);
        self.received.store(verifier.received, Ordering::SeqCst);
    }
    fn process_response_eof(&mut self, _: &Request, response: &Response) {
        let _ = self
            .sender
            .take()
            .unwrap()
            .send((response.clone(), self.verifier.take().unwrap()));
    }
    fn process_csp_violations(&mut self, _: &Request, _: Vec<csp::Violation>) {}
    fn process_response_length_hint(&mut self, _: &Request, _: usize) {}
}

/// A fetch started with `start_streaming_fetch`.
struct StreamingFetch {
    listener: Arc<CancellationListener>,
    /// The number of body bytes received so far.
    received: Arc<AtomicUsize>,
    end: tokio::sync::oneshot::Receiver<(Response, StreamVerifier)>,
}

impl StreamingFetch {
    fn received(&self) -> usize {
        self.received.load(Ordering::SeqCst)
    }

    /// Wait for the first chunk of the body.
    fn wait_for_first_chunk(&self) {
        wait_until(|| self.received() == STREAM_CHUNK_SIZE);
    }

    /// Wait for the end of the fetch, and return its response and what arrived of the body.
    fn wait_for_end(self) -> (Response, StreamVerifier) {
        wait_for_fetch(self.end)
    }
}

/// Start a fetch of `url` in the background, the way a video element does.
fn start_streaming_fetch(url: &UrlWithBlobClaim, context: FetchContext) -> StreamingFetch {
    let (sender, end) = tokio::sync::oneshot::channel();
    let received = Arc::new(AtomicUsize::new(0));
    let listener = context.cancellation_listener.clone();
    let mut target = StreamCollector {
        sender: Some(sender),
        verifier: Some(StreamVerifier::default()),
        received: received.clone(),
    };
    let request = build_streaming_request(url).build();
    spawn_task(async move {
        methods::fetch(request, &mut target, &context).await;
    });
    StreamingFetch {
        listener,
        received,
        end,
    }
}

/// Wait until `count` has not changed for half a second, and return it.
fn wait_for_plateau(count: &AtomicUsize) -> usize {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = count.load(Ordering::SeqCst);
    let mut unchanged_since = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(20));
        let current = count.load(Ordering::SeqCst);
        if current != last {
            last = current;
            unchanged_since = Instant::now();
        } else if unchanged_since.elapsed() >= Duration::from_millis(500) {
            return current;
        }
        assert!(Instant::now() < deadline, "the server kept sending");
    }
}

fn check_suspended_fetch_holds_back_the_server(http2: bool) {
    let mut server = make_streaming_server(http2, 1, STREAM_LENGTH, false);
    let fetch = start_streaming_fetch(&server.url, new_streaming_fetch_context(&server));

    fetch.wait_for_first_chunk();
    assert!(fetch.listener.suspend());
    assert!(fetch.listener.suspend());
    server.release(0);
    let held = wait_for_plateau(&server.sent[0]);
    // Over HTTP/1.1, what the kernel buffers depends on the host; HTTP/2 holds the stream window.
    let bound = if http2 {
        STREAM_LENGTH / 4
    } else {
        STREAM_LENGTH
    };
    assert!(
        held < bound,
        "the server sent {held} bytes while the fetch was suspended"
    );
    assert_eq!(fetch.received(), STREAM_CHUNK_SIZE);

    fetch.listener.resume();
    fetch.listener.resume();
    let listener = fetch.listener.clone();
    let (response, body) = fetch.wait_for_end();
    assert!(!response.is_network_error());
    assert!(!response.aborted.load(Ordering::SeqCst));
    assert!(body.is_intact(STREAM_LENGTH));
    assert_eq!(server.sent(0), STREAM_LENGTH);
    assert_eq!(server.used_http2.load(Ordering::SeqCst), http2);
    assert!(!listener.suspend());

    let _ = server.server.close();
}

#[test]
fn test_suspended_fetch_holds_back_the_server_until_resumed() {
    check_suspended_fetch_holds_back_the_server(false);
}

#[test]
fn test_suspended_http2_fetch_holds_back_the_server_until_resumed() {
    check_suspended_fetch_holds_back_the_server(true);
}

#[test]
fn test_cancelling_a_suspended_fetch_ends_it() {
    let mut server = make_streaming_server(false, 1, STREAM_LENGTH, false);
    let fetch = start_streaming_fetch(&server.url, new_streaming_fetch_context(&server));

    fetch.wait_for_first_chunk();
    assert!(fetch.listener.suspend());
    server.release(0);
    assert!(wait_for_plateau(&server.sent[0]) < STREAM_LENGTH);

    let cancelled_at = Instant::now();
    fetch.listener.cancel();
    let (response, body) = fetch.wait_for_end();
    assert!(cancelled_at.elapsed() < Duration::from_secs(1));
    assert!(response.aborted.load(Ordering::SeqCst));
    assert_eq!(body.received, STREAM_CHUNK_SIZE);

    let _ = server.server.close();
}

#[test]
fn test_cancelling_a_fetch_that_waits_for_the_network_ends_it() {
    // The server is never released, so it sends nothing after the first chunk.
    let server = make_streaming_server(false, 1, STREAM_LENGTH, false);
    let fetch = start_streaming_fetch(&server.url, new_streaming_fetch_context(&server));

    fetch.wait_for_first_chunk();
    let cancelled_at = Instant::now();
    fetch.listener.cancel();
    let (response, body) = fetch.wait_for_end();
    assert!(cancelled_at.elapsed() < Duration::from_secs(1));
    assert!(response.aborted.load(Ordering::SeqCst));
    assert_eq!(body.received, STREAM_CHUNK_SIZE);

    let _ = server.server.close();
}

#[test]
fn test_fetch_whose_connection_closes_while_suspended_ends_when_resumed() {
    let chunks = 2;
    let (url, release, closed) = make_closing_server(chunks);
    let fetch = start_streaming_fetch(&url, new_fetch_context(None, None));

    fetch.wait_for_first_chunk();
    assert!(fetch.listener.suspend());
    release.send(()).unwrap();
    wait_until(|| closed.load(Ordering::SeqCst));
    assert_eq!(fetch.received(), STREAM_CHUNK_SIZE);

    fetch.listener.resume();
    let (response, body) = fetch.wait_for_end();
    assert!(response.aborted.load(Ordering::SeqCst));
    assert!(!body.mismatch);
    assert_eq!(body.received, chunks * STREAM_CHUNK_SIZE);
}

#[test]
fn test_fetch_suspended_for_too_long_ends_with_a_network_error() {
    let mut server = make_streaming_server(false, 3, STREAM_LENGTH, false);
    let mut context = new_streaming_fetch_context(&server);
    let mut state = create_http_state(None);
    state.suspended_fetches = Arc::new(SuspendedFetches::with_timeout(Duration::from_millis(300)));
    context.state = Arc::new(state);
    let fetches: Vec<_> = (0..2)
        .map(|_| {
            let fetch = start_streaming_fetch(&server.url, with_new_listener(&context));
            fetch.wait_for_first_chunk();
            fetch
        })
        .collect();

    let suspended_at = Instant::now();
    for (index, fetch) in fetches.iter().enumerate() {
        assert!(fetch.listener.suspend());
        server.release(index);
    }
    for fetch in fetches {
        let (response, body) = fetch.wait_for_end();
        assert_eq!(
            response.get_network_error(),
            Some(&NetworkError::ConnectionFailure)
        );
        assert_eq!(body.received, STREAM_CHUNK_SIZE);
    }
    let elapsed = suspended_at.elapsed();
    assert!(elapsed >= Duration::from_millis(300));
    assert!(elapsed < Duration::from_secs(5));

    // The fetches that ended gave up their places among the suspended fetches of the origin.
    let fetch = start_streaming_fetch(&server.url, with_new_listener(&context));
    fetch.wait_for_first_chunk();
    assert!(fetch.listener.suspend());
    fetch.listener.cancel();
    let _ = fetch.wait_for_end();

    let _ = server.server.close();
}

#[test]
fn test_at_most_two_fetches_per_origin_are_suspended() {
    let mut server = make_streaming_server(false, 5, STREAM_LENGTH, false);
    let context = new_streaming_fetch_context(&server);
    let start = |server: &StreamingServer| {
        let fetch = start_streaming_fetch(&server.url, with_new_listener(&context));
        fetch.wait_for_first_chunk();
        fetch
    };
    let first = start(&server);
    let second = start(&server);
    let third = start(&server);

    assert!(first.listener.suspend());
    assert!(second.listener.suspend());
    assert!(!third.listener.suspend());
    for index in 0..3 {
        server.release(index);
    }
    let (_, body) = third.wait_for_end();
    assert!(body.is_intact(STREAM_LENGTH));
    assert!(wait_for_plateau(&server.sent[0]) < STREAM_LENGTH);
    assert!(wait_for_plateau(&server.sent[1]) < STREAM_LENGTH);

    // Cancelling and resuming give up the places of the suspended fetches.
    first.listener.cancel();
    second.listener.resume();
    let fourth = start(&server);
    let fifth = start(&server);
    assert!(fourth.listener.suspend());
    assert!(fifth.listener.suspend());

    let (response, _) = first.wait_for_end();
    assert!(response.aborted.load(Ordering::SeqCst));
    let (_, body) = second.wait_for_end();
    assert!(body.is_intact(STREAM_LENGTH));
    for fetch in [fourth, fifth] {
        fetch.listener.cancel();
        let _ = fetch.wait_for_end();
    }

    let _ = server.server.close();
}

#[test]
fn test_fetch_waiting_for_a_suspended_response_uses_the_network() {
    // Small enough for the HTTP cache to store the response to a video request.
    let length = 16 * STREAM_CHUNK_SIZE;
    let mut server = make_streaming_server(false, 3, length, true);
    let context = new_streaming_fetch_context(&server);
    let request = build_streaming_request(&server.url).build();
    let producer = start_streaming_fetch(&server.url, context.clone());
    producer.wait_for_first_chunk();
    assert_eq!(stored_response_count(&context, &request), 1);

    // A fetch that waits for the stored body when its producer is suspended...
    let waiting = start_streaming_fetch(&server.url, with_new_listener(&context));
    wait_until(|| awaiting_consumer_count(&context, &request) == 1);
    assert!(producer.listener.suspend());
    server.release(1);
    let (response, body) = waiting.wait_for_end();
    assert!(matches!(response.cache_state, CacheState::None));
    assert!(body.is_intact(length));

    // ...and one that starts while the producer is suspended, go to the network.
    let later = start_streaming_fetch(&server.url, with_new_listener(&context));
    server.release(2);
    let (response, body) = later.wait_for_end();
    assert!(matches!(response.cache_state, CacheState::None));
    assert!(body.is_intact(length));
    assert_eq!(server.sent(1), length);
    assert_eq!(server.sent(2), length);

    producer.listener.resume();
    server.release(0);
    let (_, body) = producer.wait_for_end();
    assert!(body.is_intact(length));

    let _ = server.server.close();
}

/// Fetch a `data:` URL through the fetch thread and `resource_thread`, and wait for its body.
fn fetch_data_url(resource_thread: &CoreResourceThread) -> Vec<u8> {
    let url = UrlWithBlobClaim::new(ServoUrl::parse("data:,data").unwrap(), None);
    let request = RequestBuilder::new(None, url.clone(), Referrer::NoReferrer)
        .origin(url.origin())
        .policy_container(Default::default());
    let (sender, receiver) = unbounded();
    fetch_async(
        resource_thread,
        request,
        None,
        Box::new(move |message| {
            let _ = sender.send(message);
        }),
    );
    let mut body = Vec::new();
    loop {
        match receiver.recv_timeout(Duration::from_secs(10)).unwrap() {
            FetchResponseMsg::ProcessResponseChunk(_, chunk) => body.extend_from_slice(&chunk),
            FetchResponseMsg::ProcessResponseEOF(_, result, _) => {
                assert!(result.is_ok(), "{result:?}");
                return body;
            },
            _ => {},
        }
    }
}

#[test]
fn test_fetch_controller_suspends_and_resumes_a_fetch() {
    let (time_sender, _time_receiver) = generic_channel::channel().unwrap();
    let (memory_sender, _memory_receiver) = generic_channel::channel().unwrap();
    let (resource_thread, _private_resource_thread) = new_core_resource_thread(
        None,
        ProfilerChan(Some(time_sender)),
        MemProfilerChan(memory_sender),
        create_generic_embedder_proxy(),
        None,
        CACertificates::Default,
        false, /* ignore_certificate_errors */
        Arc::new(ProtocolRegistry::with_internal_protocols()),
    );

    let mut server = make_streaming_server(false, 1, STREAM_LENGTH, false);
    let request = build_streaming_request(&server.url);
    let controller = FetchController::new(request.id, false, resource_thread.clone());
    let (sender, receiver) = unbounded();
    fetch_async(
        &resource_thread,
        request,
        None,
        Box::new(move |message| {
            let _ = sender.send(message);
        }),
    );

    let mut verifier = StreamVerifier::default();
    let receive = |verifier: &mut StreamVerifier| loop {
        match receiver.recv_timeout(Duration::from_secs(10)).unwrap() {
            FetchResponseMsg::ProcessResponseChunk(_, chunk) => {
                verifier.verify(&chunk);
                return false;
            },
            FetchResponseMsg::ProcessResponseEOF(_, result, _) => {
                assert!(result.is_ok());
                return true;
            },
            _ => {},
        }
    };
    while verifier.received < STREAM_CHUNK_SIZE {
        assert!(!receive(&mut verifier));
    }
    controller.suspend();
    // Net handles messages from the fetch thread in order, so it handled the suspension once a
    // later fetch has ended.
    assert_eq!(fetch_data_url(&resource_thread), b"data");
    server.release(0);
    assert!(wait_for_plateau(&server.sent[0]) < STREAM_LENGTH);
    assert_eq!(verifier.received, STREAM_CHUNK_SIZE);

    controller.resume();
    while !receive(&mut verifier) {}
    assert!(verifier.is_intact(STREAM_LENGTH));

    // After the end, both do nothing: no message arrives and net keeps working.
    controller.suspend();
    controller.resume();
    controller.suspend();
    assert_eq!(fetch_data_url(&resource_thread), b"data");
    assert!(receiver.try_recv().is_err());

    let _ = server.server.close();
    let (exit_sender, exit_receiver) = generic_channel::oneshot().unwrap();
    resource_thread
        .send(CoreResourceMsg::Exit(exit_sender))
        .unwrap();
    exit_receiver.recv().unwrap();
}
