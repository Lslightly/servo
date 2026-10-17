/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use headers::{ContentLength, ContentRange, HeaderMapExt, Range};
use http::StatusCode;
use http::header::{CONTENT_ENCODING, ETAG, HeaderMap, HeaderValue, IF_RANGE, LAST_MODIFIED};
use js::context::JSContext;
use net_traits::fetch::fetch_controller::FetchController;
use net_traits::request::{Destination, RequestBuilder, RequestId};
use net_traits::{
    CoreResourceThread, FetchMetadata, FetchResponseMsg, Metadata, NetworkError,
    ResourceFetchTiming, fetch_async,
};
use servo_base::generic_channel::GenericCallback;
use servo_media_cache::{
    FetchError, FetchHandle, FetchRequest, FetchSink, Fetcher, ResponseInfo, Validator,
};
use servo_url::ServoUrl;

use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::DomRoot;
use crate::dom::csp::GlobalCspReporting;
use crate::dom::element::{Element, cors_setting_for_element, referrer_policy_for_element};
use crate::dom::globalscope::GlobalScope;
use crate::dom::html::htmlmediaelement::{HTMLMediaElement, MediaElementTaskQueue};
use crate::dom::html::htmlvideoelement::HTMLVideoElement;
use crate::dom::node::NodeTraits;
use crate::dom::performance::performanceresourcetiming::InitiatorType;
use crate::fetch::fetch::{RequestWithGlobalScope, create_a_potential_cors_request_with_claim};
use crate::fetch::network_listener::{self, ResourceTimingListener};
use crate::url::ensure_blob_referenced_by_url_is_kept_alive;

/// Runs on a media element when the first fetch of a [`ScriptFetcher`] fails.
pub(crate) type FetchFailureSteps = Box<dyn Fn(&mut JSContext, &HTMLMediaElement) + Send + Sync>;

/// Fetches a media resource for the media cache on behalf of a media element, from any thread.
/// The fetches report to the cache on the `FetchThread`, and queue tasks on the element only for
/// what script does: CSP violation reports, resource timing entries, and the failure of the first
/// fetch.
pub(crate) struct ScriptFetcher {
    /// The request that every fetch starts from, as the media element builds it.
    request: RequestBuilder,
    core_resource_thread: Mutex<CoreResourceThread>,
    element: Arc<ElementTasks>,
    /// Whether no fetch was started yet.
    first_fetch: AtomicBool,
}

impl ScriptFetcher {
    /// Builds the fetcher of `url` for `element`, as the resource fetch algorithm does. Returns
    /// None if the element has no player, which its tasks need.
    /// <https://html.spec.whatwg.org/multipage/#concept-media-load-resource>
    pub(crate) fn new(
        element: &HTMLMediaElement,
        url: ServoUrl,
        first_fetch_failure_steps: FetchFailureSteps,
    ) -> Option<Self> {
        let global = element.global();
        let tasks =
            element.task_queue(global.task_manager().networking_task_source().to_sendable())?;
        // Step 5.remote.2. Let destination be "audio" if the media element is an audio element,
        // or "video" otherwise.
        let destination = if element.is::<HTMLVideoElement>() {
            Destination::Video
        } else {
            Destination::Audio
        };
        // Step 5.remote.3. Let request be the result of creating a potential-CORS request given
        // current media resource's URL record, destination, and the current state of the media
        // element's crossorigin content attribute. The request keeps the blob of a blob URL alive,
        // so that later fetches of the resource work after the URL is revoked.
        let request = create_a_potential_cors_request_with_claim(
            Some(element.owner_document().webview_id()),
            ensure_blob_referenced_by_url_is_kept_alive(&global, url.clone()),
            destination,
            cors_setting_for_element(element.upcast()),
            None,
            global.get_referrer(),
        )
        // Step 5.remote.4. Set request's client to the media element's node document's relevant
        // settings object.
        .with_global_scope(&global)
        .referrer_policy(referrer_policy_for_element(element.upcast()));
        Some(Self {
            request,
            core_resource_thread: Mutex::new(global.core_resource_thread()),
            element: Arc::new(ElementTasks {
                tasks,
                url,
                first_fetch_failure_steps,
            }),
            first_fetch: AtomicBool::new(true),
        })
    }
}

impl Fetcher for ScriptFetcher {
    fn start(&self, request: FetchRequest, sink: Arc<dyn FetchSink>) -> Box<dyn FetchHandle> {
        let mut builder = self.request.clone();
        builder.id = RequestId::default();
        builder.url = builder.url.with_new_token();
        // Steps 5.remote.6 and 5.remote.7: the cache picks the byte range.
        if let Ok(range) = Range::bytes(request.start..) {
            builder.headers.typed_insert(range);
        }
        if let Some(value) = request.if_range.as_ref().and_then(if_range_value) {
            builder.headers.insert(IF_RANGE, value);
        }

        let core_resource_thread = self.core_resource_thread.lock().unwrap().clone();
        let fetch = Arc::new(ScriptFetch {
            sink,
            controller: Mutex::new(FetchController::new(
                builder.id,
                false,
                core_resource_thread.clone(),
            )),
            ended: AtomicBool::new(false),
        });
        let mut listener = ScriptFetchListener {
            fetch: fetch.clone(),
            element: self.element.clone(),
            first: self.first_fetch.swap(false, Ordering::AcqRel),
            responded: false,
        };
        fetch_async(
            &core_resource_thread,
            builder,
            None,
            Box::new(move |message| listener.process(message)),
        );
        Box::new(ScriptFetchHandle(fetch))
    }
}

/// What the fetches of a [`ScriptFetcher`] ask of its media element.
struct ElementTasks {
    tasks: MediaElementTaskQueue,
    url: ServoUrl,
    first_fetch_failure_steps: FetchFailureSteps,
}

/// The state of one fetch, shared by its handle and by its listener on the `FetchThread`.
struct ScriptFetch {
    sink: Arc<dyn FetchSink>,
    controller: Mutex<FetchController>,
    /// Whether the sink heard the end of the fetch, or the fetch was cancelled. The sink hears
    /// nothing after that.
    ended: AtomicBool,
}

impl ScriptFetch {
    fn is_ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }

    /// Reports the end of the fetch, and aborts the fetch if it failed, so that net stops
    /// reading the body of an error response.
    fn end(&self, result: Result<(), FetchError>) {
        if self.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        {
            let mut controller = self.controller.lock().unwrap();
            if result.is_ok() {
                controller.ignore();
            } else {
                controller.abort();
            }
        }
        self.sink.end(result);
    }

    fn cancel(&self) {
        if !self.ended.swap(true, Ordering::AcqRel) {
            self.controller.lock().unwrap().abort();
        }
    }
}

struct ScriptFetchHandle(Arc<ScriptFetch>);

impl FetchHandle for ScriptFetchHandle {
    fn suspend(&self) {
        let sink = self.0.sink.clone();
        let Ok(reply) = GenericCallback::new(move |suspended: Result<bool, _>| {
            sink.suspended(suspended.unwrap_or(false));
        }) else {
            warn!("Could not ask net for the suspension of a media fetch");
            self.0.sink.suspended(false);
            return;
        };
        self.0.controller.lock().unwrap().suspend(Some(reply));
    }

    fn resume(&self) {
        self.0.controller.lock().unwrap().resume();
    }

    fn cancel(&self) {
        self.0.cancel();
    }
}

impl Drop for ScriptFetchHandle {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Receives the messages of one fetch on the `FetchThread`.
struct ScriptFetchListener {
    fetch: Arc<ScriptFetch>,
    element: Arc<ElementTasks>,
    /// Whether this is the first fetch of its fetcher.
    first: bool,
    /// Whether the response was passed to the sink.
    responded: bool,
}

impl ScriptFetchListener {
    fn process(&mut self, message: FetchResponseMsg) {
        match message {
            FetchResponseMsg::ProcessResponse(_, Ok(metadata)) => self.process_response(metadata),
            FetchResponseMsg::ProcessResponse(_, Err(error)) => self.fail(fetch_error(&error)),
            FetchResponseMsg::ProcessResponseChunk(_, chunk) => self.process_chunk(chunk),
            FetchResponseMsg::ProcessResponseEOF(_, result, timing) => {
                self.submit_timing(result.clone(), timing);
                match result {
                    Ok(()) if self.responded => self.fetch.end(Ok(())),
                    Ok(()) => self.fail(FetchError::Network),
                    Err(error) => self.fail(fetch_error(&error)),
                }
            },
            FetchResponseMsg::ProcessCspViolations(_, violations) => {
                self.element.tasks.queue(move |cx, element| {
                    element
                        .owner_global()
                        .report_csp_violations(cx, violations, None, None);
                });
            },
            FetchResponseMsg::ProcessRequestBody(_) |
            FetchResponseMsg::ProcessContentLength(..) => {},
        }
    }

    fn process_response(&mut self, fetch_metadata: FetchMetadata) {
        let cors_tainted = fetch_metadata.is_cors_cross_origin();
        let metadata: Metadata = fetch_metadata.into();
        let status = metadata.status.raw_code();
        if status == StatusCode::RANGE_NOT_SATISFIABLE.as_u16() {
            self.fail(FetchError::RangeNotSatisfiable);
            return;
        }
        // <https://html.spec.whatwg.org/multipage/#verify-a-media-response>
        // Step 4. If internalResponse's status is 200, then return true.
        // Step 5. If internalResponse's status is not 206, then return false.
        // Step 6. If the result of extracting content-range values from internalResponse is
        // failure, then return false.
        // The media cache runs the remaining steps, which compare the origins of the responses.
        let partial = status == StatusCode::PARTIAL_CONTENT.as_u16();
        if status != StatusCode::OK.as_u16() && !partial {
            self.fail(FetchError::Status(status));
            return;
        }
        let byte_range = metadata
            .headers
            .as_deref()
            .and_then(HeaderMapExt::typed_get::<ContentRange>)
            .and_then(|content_range| content_range.bytes_range());
        let start = match byte_range {
            Some((start, _end)) if partial => start,
            None if partial => {
                self.fail(FetchError::Network);
                return;
            },
            _ => 0,
        };
        if self.fetch.is_ended() {
            return;
        }
        self.responded = true;
        let origin = metadata
            .final_url
            .origin()
            .ascii_serialization()
            .into_owned();
        self.fetch.sink.response(response_info(
            metadata.headers.as_deref(),
            partial,
            start,
            cors_tainted,
            origin,
        ));
    }

    fn process_chunk(&mut self, chunk: Bytes) {
        if self.responded && !chunk.is_empty() && !self.fetch.is_ended() {
            self.fetch.sink.data(chunk);
        }
    }

    /// Ends a fetch that failed. If it is the first fetch of its fetcher and failed before its
    /// response, the media data cannot be fetched at all, which the element learns even if the
    /// cache recovers for other readers.
    fn fail(&mut self, error: FetchError) {
        if self.fetch.is_ended() {
            return;
        }
        if self.first && !self.responded {
            let element_tasks = self.element.clone();
            self.element.tasks.queue(move |cx, element| {
                (element_tasks.first_fetch_failure_steps)(cx, element);
            });
        }
        self.fetch.end(Err(error));
    }

    fn submit_timing(&self, result: Result<(), NetworkError>, timing: ResourceFetchTiming) {
        let url = self.element.url.clone();
        self.element.tasks.queue(move |cx, element| {
            let listener = MediaFetchTiming {
                element: DomRoot::from_ref(element),
                url,
            };
            network_listener::submit_timing(cx, &listener, &result, &timing);
        });
    }
}

/// The resource timing of a media fetch.
struct MediaFetchTiming {
    element: DomRoot<HTMLMediaElement>,
    url: ServoUrl,
}

impl ResourceTimingListener for MediaFetchTiming {
    fn resource_timing_information(&self) -> (InitiatorType, ServoUrl) {
        let initiator_type =
            InitiatorType::LocalName(self.element.upcast::<Element>().local_name().to_string());
        (initiator_type, self.url.clone())
    }

    fn resource_timing_global(&self) -> DomRoot<GlobalScope> {
        self.element.owner_document().global()
    }
}

/// A request that CSP, mixed content or a CORS check blocked fails for good.
fn fetch_error(error: &NetworkError) -> FetchError {
    if error.is_permanent_failure() {
        FetchError::Blocked
    } else {
        FetchError::Network
    }
}

fn response_info(
    headers: Option<&HeaderMap>,
    partial: bool,
    start: u64,
    cors_tainted: bool,
    origin: String,
) -> ResponseInfo {
    let encoded = headers.is_some_and(|headers| headers.contains_key(CONTENT_ENCODING));
    let content_range = headers.and_then(HeaderMapExt::typed_get::<ContentRange>);
    // The Content-Length of an encoded body is not the length of the decoded body.
    let total_length = if encoded {
        None
    } else if partial {
        content_range.and_then(|content_range| content_range.bytes_len())
    } else {
        headers
            .and_then(HeaderMapExt::typed_get::<ContentLength>)
            .map(|content_length| content_length.0)
    };
    ResponseInfo {
        partial,
        start,
        total_length,
        validator: headers.and_then(validator),
        encoded,
        cors_tainted,
        origin: Some(origin),
    }
}

/// The strong validator of a response: its ETag, unless that is weak, since a weak ETag does not
/// validate a range, or else its Last-Modified date.
fn validator(headers: &HeaderMap) -> Option<Validator> {
    let header = |name| headers.get(name)?.to_str().ok().map(str::to_owned);
    header(ETAG)
        .filter(|etag| !etag.starts_with("W/"))
        .map(Validator::ETag)
        .or_else(|| header(LAST_MODIFIED).map(Validator::LastModified))
}

fn if_range_value(validator: &Validator) -> Option<HeaderValue> {
    let value = match validator {
        Validator::ETag(value) | Validator::LastModified(value) => value,
    };
    HeaderValue::from_str(value).ok()
}
