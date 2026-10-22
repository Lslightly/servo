/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#![deny(missing_docs)]

//! Servo's HTTP cache.
//!
//! The RFC 9111 rules — storability, freshness, `Vary`, revalidation and the
//! 304 merge — come from `http-cache-rules`, configured in [`policy`]. On top of
//! them this module keeps the parts of caching that belong to the fetch layer:
//! the fetch cache modes, background revalidation, invalidation after unsafe
//! methods and range requests.
//!
//! Storage lives behind [`CacheStore`], which streams bodies in and out and never
//! hands out a complete body, so a cached response does not have to exist as a
//! single allocation in the net process.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::SystemTime;

use futures::StreamExt;
use headers::HeaderMapExt;
use http::{HeaderMap, Method, StatusCode, header};
use http_cache_rules::{
    Exchange, FreshenError, Reuse, StoredPolicy, VaryKey, invalidation_candidates,
};
use log::debug;
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use malloc_size_of_derive::MallocSizeOf as MallocSizeOfDerive;
use net_traits::body_stream::{BodyEnd, BodySender, body_channel};
use net_traits::http_status::HttpStatus;
use net_traits::request::{CacheMode, Request, RequestMode};
use net_traits::response::{CacheState, Response, ResponseBody};
use net_traits::{CacheEntryDescriptor, NetworkError, ResourceFetchTiming};
use servo_config::pref;
use tokio::sync::watch;

use crate::connector::BoxedBody;
use crate::decoder::{Decoder, DecoderType};
use crate::fetch::methods::FetchFlowState;
use crate::http_cache::inflight::{InFlight, InFlightState, InFlightWriter, RevalidationGuards};
use crate::http_cache::key::EntryId;
use crate::http_cache::memory_store::MemoryStore;
use crate::http_cache::policy::{CONFIG, EntryPolicy, may_store_media_response};
use crate::http_cache::range::RangeOutcome;
use crate::http_cache::store::{BodyReader, CACHE_FORMAT, CacheStore, EntryMeta};
use crate::http_cache::tee::{TeeBody, TeeCommit};

pub mod disk;
mod inflight;
mod key;
pub mod memory_store;
pub mod policy;
mod range;
pub mod store;
mod tee;

pub use crate::http_cache::inflight::RevalidationGuard;
pub use crate::http_cache::key::CacheKey;

/// How many times a lookup will wait for another fetch to finish writing the same
/// key before giving up and going to the network itself.
const MAX_INFLIGHT_WAITS: usize = 8;

/// Is this cache the private or the public one?
#[derive(Clone, Copy, Debug, MallocSizeOfDerive, PartialEq)]
pub enum HttpCacheAssignment {
    /// The cache shared by ordinary browsing, which may be backed by disk.
    Public,
    /// The private-browsing cache, which is always memory-only.
    Private,
}

/// Servo's HTTP cache.
pub struct HttpCache {
    store: Arc<dyn CacheStore>,
    inflight: Arc<InFlight>,
    revalidations: Arc<RevalidationGuards>,
    /// Which side of the browsing session this cache serves.
    assignment: HttpCacheAssignment,
}

impl MallocSizeOf for HttpCache {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        self.store.size_of(ops)
    }
}

impl HttpCache {
    /// Create a cache for the given side of the browsing session. The public cache
    /// is backed by disk when the embedder supplied a cache directory; everything
    /// else is memory-only.
    pub fn new(assignment: HttpCacheAssignment) -> Self {
        let memory_budget = pref!(network_http_memory_cache_size).max(0) as usize;
        let store: Arc<dyn CacheStore> = match assignment {
            HttpCacheAssignment::Public => disk::open_default_store()
                .unwrap_or_else(|| Arc::new(MemoryStore::new(memory_budget))),
            HttpCacheAssignment::Private => Arc::new(MemoryStore::new(memory_budget)),
        };
        Self {
            store,
            inflight: Arc::new(InFlight::default()),
            revalidations: Arc::new(RevalidationGuards::default()),
            assignment,
        }
    }

    /// A cache backed by an explicit store. Used by the store test suite.
    pub fn with_store(store: Arc<dyn CacheStore>) -> Self {
        Self {
            store,
            inflight: Arc::new(InFlight::default()),
            revalidations: Arc::new(RevalidationGuards::default()),
            assignment: HttpCacheAssignment::Public,
        }
    }

    /// The bytes this cache holds in the process, for `about:memory`.
    pub fn stored_bytes(&self) -> usize {
        self.store.stored_bytes()
    }

    /// The bytes this cache holds on disk, for `about:memory`.
    pub fn disk_bytes(&self) -> u64 {
        self.store.disk_bytes()
    }

    /// Whether this is the public cache.
    pub fn assignment(&self) -> HttpCacheAssignment {
        self.assignment
    }

    /// Descriptors for the entries devtools lists.
    pub(crate) async fn cache_entry_descriptors(&self) -> Vec<CacheEntryDescriptor> {
        self.store.descriptors().await
    }

    /// Drop everything this cache holds.
    pub(crate) async fn clear(&self) {
        self.store.clear().await;
    }

    /// Persist anything that only lives in memory. Called when the embedding
    /// application is backgrounded, since it may be killed without further notice.
    pub(crate) async fn flush(&self) {
        self.store.flush().await;
    }

    /// Flush anything that only lives in memory. Called when the resource thread exits.
    pub(crate) async fn shutdown(&self) {
        self.store.shutdown().await;
    }

    /// Consult the cache before going to the network.
    ///
    /// Implements the cache half of step 8.26 of
    /// <https://fetch.spec.whatwg.org/#concept-http-network-or-cache-fetch>: it
    /// selects a stored response, applies the request's cache mode to it, and sets
    /// the conditional headers when the stored response has to be revalidated.
    ///
    /// `producer` is the flow state of this fetch, which a fetch that waits for it
    /// to store the response watches.
    ///
    /// The returned [`CacheTransaction`] carries what the rest of the fetch needs:
    /// the entry being revalidated and, on a miss, the right to write this key.
    pub(crate) async fn read(
        &self,
        request: &mut Request,
        producer: watch::Receiver<FetchFlowState>,
        revalidating_flag: &mut bool,
    ) -> (CacheTransaction, CacheLookup) {
        let mut transaction = self.new_transaction(request, producer);
        let key = transaction.key.clone();
        let mut lookup = CacheLookup::default();

        if !transaction.storable || request.cache_mode == CacheMode::NoStore {
            return (transaction, lookup);
        }
        if request.cache_mode == CacheMode::Reload {
            // No stored response is used, but the response replaces the ones that
            // this request matches.
            let now = SystemTime::now();
            for (id, meta) in self.store.lookup(&key).await {
                if suitability(request, &meta, now).is_some() {
                    transaction.superseded.push((id, meta.stamp));
                }
            }
            transaction.writer = self
                .inflight
                .register(&key, transaction.producer.clone())
                .ok();
            return (transaction, lookup);
        }

        if let Some((id, meta, reuse)) = self.select_variant(&mut transaction, request).await {
            match stored_response_use(request, reuse) {
                StoredResponseUse::None => {},
                StoredResponseUse::Validate => {
                    // Substep 2.2: revalidate before use. The preconditions are those
                    // of this entry only, so that a 304 is about it.
                    *revalidating_flag = true;
                    request.headers.remove(header::IF_NONE_MATCH);
                    request.headers.remove(header::IF_MODIFIED_SINCE);
                    for (name, value) in meta.policy.validation_fields() {
                        request.headers.insert(name, value);
                    }
                    transaction.added_preconditions = true;
                    transaction.revalidating = Some((id, meta));
                },
                usage @ (StoredResponseUse::Serve | StoredResponseUse::ServeAndRevalidate) => {
                    let headers = meta
                        .policy
                        .presented_fields(&meta.headers, SystemTime::now());
                    match self
                        .serve(request, id, &meta, headers, CacheState::Local)
                        .await
                    {
                        Ok(response) => {
                            self.store.touch(id);
                            if usage == StoredResponseUse::ServeAndRevalidate {
                                lookup.revalidate_in_background =
                                    self.revalidations.try_acquire(&key);
                            }
                            lookup.response = Some(response);
                        },
                        Err(ServeError::Gone) => {
                            // The entry vanished between the lookup and the open, which
                            // the store is allowed to do. Fall back to the network.
                            self.store.remove(id, meta.stamp).await;
                        },
                        Err(ServeError::RangeNotSatisfiable) => {
                            // Keep the entry: this request just cannot be answered from it.
                        },
                    }
                },
            }
        }

        if lookup.response.is_none() && transaction.writer.is_none() && !transaction.cancelled() {
            transaction.writer = self
                .inflight
                .register(&transaction.key, transaction.producer.clone())
                .ok();
        }
        (transaction, lookup)
    }

    fn new_transaction(
        &self,
        request: &Request,
        producer: watch::Receiver<FetchFlowState>,
    ) -> CacheTransaction {
        CacheTransaction {
            key: CacheKey::new(request),
            superseded: Vec::new(),
            revalidating: None,
            writer: None,
            storable: is_cacheable_request(request),
            producer,
            request_time: None,
            added_preconditions: false,
        }
    }

    /// Find the stored variant that answers `request`, waiting for a concurrent
    /// fetch of the same key if one is already writing it.
    async fn select_variant(
        &self,
        transaction: &mut CacheTransaction,
        request: &Request,
    ) -> Option<(EntryId, EntryMeta, Reuse)> {
        for _ in 0..MAX_INFLIGHT_WAITS {
            let now = SystemTime::now();
            let mut selected: Option<(EntryId, EntryMeta, Reuse)> = None;
            for (id, meta) in self.store.lookup(&transaction.key).await {
                let Some(reuse) = suitability(request, &meta, now) else {
                    continue;
                };
                transaction.superseded.push((id, meta.stamp));
                // RFC 9111 section 4: "When more than one suitable response is
                // stored, a cache MUST use the most recent one (as determined by the
                // Date header field)."
                let preferred = selected.as_ref().is_none_or(|(_, chosen, chosen_reuse)| {
                    (meta.policy.date(), preference(reuse)) >
                        (chosen.policy.date(), preference(*chosen_reuse))
                });
                if preferred {
                    selected = Some((id, meta, reuse));
                }
            }
            if selected.is_some() || transaction.cancelled() {
                return selected;
            }

            // Nothing usable. Either become the writer for this key, or wait for
            // whoever already is.
            match self
                .inflight
                .register(&transaction.key, transaction.producer.clone())
            {
                Ok(writer) => {
                    transaction.writer = Some(writer);
                    return None;
                },
                Err(waiter) => {
                    match inflight::wait(waiter, transaction.producer.clone(), &request.headers)
                        .await
                    {
                        InFlightState::Committed(_) | InFlightState::Aborted => continue,
                        // Nothing to wait for, the writer is suspended, which may last
                        // long, or the wait ended: go to the network without storing
                        // the response.
                        InFlightState::NotShared |
                        InFlightState::Writing |
                        InFlightState::Committing => {
                            return None;
                        },
                    }
                },
            }
        }
        None
    }

    /// Build a response that streams a stored entry with `headers`.
    async fn serve(
        &self,
        request: &Request,
        id: EntryId,
        meta: &EntryMeta,
        mut headers: HeaderMap,
        cache_state: CacheState,
    ) -> Result<Response, ServeError> {
        let mut status = meta.status.clone();

        let body_range = match range::select(request, meta) {
            RangeOutcome::Whole => None,
            RangeOutcome::Unsatisfiable => return Err(ServeError::RangeNotSatisfiable),
            RangeOutcome::Satisfiable(hit) => {
                range::apply_headers(&mut headers, &hit);
                status = StatusCode::PARTIAL_CONTENT.into();
                Some(hit.body)
            },
        };

        let reader = match self.store.open(id, meta, body_range).await {
            Ok(reader) => reader,
            Err(error) => {
                debug!("could not open cache entry for {}: {error}", meta.key.url());
                return Err(ServeError::Gone);
            },
        };

        let resource_timing = ResourceFetchTiming::new(request.timing_type());
        // The response's URL, URL list and `Location` come from the request, as for a
        // network response, so that the entry holds no credentials of the URL.
        let mut response = Response::new(request.current_url(), resource_timing);
        response.status = status;
        response.headers = headers;
        response.referrer = request.referrer.to_url().cloned();
        response.referrer_policy = request.referrer_policy;
        response.cache_state = cache_state;

        let length_hint = reader
            .content_encoding()
            .is_none()
            .then(|| reader.len() as usize);
        let (sender, body) = body_channel(length_hint);
        response.body = ResponseBody::Stream(body);
        spawn_entry_stream(reader, sender);
        Ok(response)
    }

    /// Freshening a stored response upon validation.
    /// <https://httpwg.org/specs/rfc9111.html#freshening.responses>
    ///
    /// Returns `None` when the stored response cannot answer the request after
    /// all, and the caller makes it again without preconditions.
    pub(crate) async fn refresh(
        &self,
        transaction: &mut CacheTransaction,
        request: &Request,
        forward_response: &Response,
    ) -> Option<Response> {
        let (id, meta) = transaction.revalidating.take()?;
        let request_headers = transaction.presented_request_headers(request);
        let exchange = Exchange {
            method: &request.method,
            request_headers: &request_headers,
            status: forward_response.status.try_code()?,
            response_headers: &forward_response.headers,
            request_time: transaction.request_time(),
            response_time: SystemTime::now(),
        };
        let freshened = match meta.policy.freshen(&CONFIG, &meta.headers, &exchange) {
            Ok(freshened) => freshened,
            Err(FreshenError::ValidatorMismatch) => {
                // The 304 is about another response than the stored one.
                self.store.remove(id, meta.stamp).await;
                return None;
            },
            Err(FreshenError::NotNotModified) => return None,
        };

        let Ok(policy) = freshened.policy else {
            // The 304 forbids storing the response, with `no-store` for example: it
            // answers this request, and the entry goes.
            let response = self
                .serve(request, id, &meta, freshened.headers, CacheState::Validated)
                .await
                .ok();
            self.store.remove(id, meta.stamp).await;
            return response;
        };
        let mut refreshed = meta;
        refreshed.policy = EntryPolicy(policy);
        refreshed.headers = freshened.headers.into();
        if let Err(error) = self.store.update_meta(id, refreshed.clone()).await {
            debug!("could not refresh cache entry: {error}");
            return None;
        }
        self.store.touch(id);
        let headers = refreshed
            .policy
            .presented_fields(&refreshed.headers, SystemTime::now());
        self.serve(request, id, &refreshed, headers, CacheState::Validated)
            .await
            .ok()
    }

    /// Copy a network body into the cache as it is delivered, when the response may
    /// be stored. Returns the body to hand on to the decoder, and what publishes the
    /// entry once that body has been decoded to its end.
    ///
    /// <https://httpwg.org/specs/rfc9111.html#storing.responses.in.caches>
    pub(crate) async fn tee(
        &self,
        transaction: &mut CacheTransaction,
        request: &Request,
        status: &HttpStatus,
        headers: &HeaderMap,
        body: BoxedBody,
    ) -> (BoxedBody, Option<TeeCommit>) {
        // A 304 freshens the entry it validates. If it cannot, the request is made
        // again, and that response is stored with the right to write this key.
        if transaction.revalidating.is_some() && *status == StatusCode::NOT_MODIFIED {
            return (body, None);
        }
        let Some(inflight) = transaction.writer.take() else {
            return (body, None);
        };
        let Some(meta) = self.entry_meta(transaction, request, status, headers) else {
            inflight.abort();
            return (body, None);
        };
        // Other fetches of the key wait for a fresh response instead of fetching it
        // too, if it varies the way their request does. One that needs validation
        // they could not use as it is.
        inflight.set_vary(meta.policy.vary().clone());
        let request_headers = transaction.presented_request_headers(request);
        let reuse = meta
            .policy
            .evaluate(&request.method, &request_headers, SystemTime::now());
        if matches!(reuse, Some(Reuse::Fresh)) {
            inflight.share();
        }

        // A stored response replaces the variants this request already matched.
        for (id, stamp) in std::mem::take(&mut transaction.superseded) {
            self.store.remove(id, stamp).await;
        }

        match self.store.create(meta).await {
            Ok(writer) => {
                let (body, commit) =
                    TeeBody::new(body, writer, inflight, self.store.max_entry_bytes());
                (http_body_util::BodyExt::boxed(body), Some(commit))
            },
            Err(error) => {
                debug!("cache store declined the entry: {error}");
                inflight.abort();
                (body, None)
            },
        }
    }

    /// The metadata a response would be stored with, or `None` if it must not be stored.
    fn entry_meta(
        &self,
        transaction: &CacheTransaction,
        request: &Request,
        status: &HttpStatus,
        headers: &HeaderMap,
    ) -> Option<EntryMeta> {
        if !transaction.storable || request.cache_mode == CacheMode::NoStore {
            return None;
        }
        let code = status.try_code()?;
        // A partial response is only worth storing if it says which part it holds.
        if code == StatusCode::PARTIAL_CONTENT && !headers.contains_key(header::CONTENT_RANGE) {
            return None;
        }
        if !may_store_media_response(request, code, headers) {
            return None;
        }
        // Refuse outsized entries up front rather than streaming one into a store
        // that will decline it at commit time anyway.
        if let Some(length) = headers.typed_get::<headers::ContentLength>() &&
            length.0 > self.store.max_entry_bytes()
        {
            return None;
        }

        let request_headers = transaction.presented_request_headers(request);
        let exchange = Exchange {
            method: &request.method,
            request_headers: &request_headers,
            status: code,
            response_headers: headers,
            request_time: transaction.request_time(),
            response_time: SystemTime::now(),
        };
        let policy = StoredPolicy::new(&CONFIG, &exchange).ok()?;
        // `Vary: *` never matches a later request, so such an entry could only ever
        // occupy a slot under its key.
        if *policy.vary() == VaryKey::Never {
            return None;
        }

        Some(EntryMeta {
            format: CACHE_FORMAT,
            stamp: rand::random(),
            key: transaction.key.clone(),
            headers: StoredPolicy::stored_fields(&CONFIG, &exchange).into(),
            policy: EntryPolicy(policy),
            content_encoding: DecoderType::detect(headers),
            status: status.clone(),
            body_len: 0,
        })
    }

    /// Invalidating stored responses after an unsafe method.
    /// <https://httpwg.org/specs/rfc9111.html#invalidation>
    pub(crate) async fn invalidate(
        &self,
        transaction: &CacheTransaction,
        request: &Request,
        response: &Response,
    ) {
        self.store.remove_key(&transaction.key).await;
        let origin = transaction.key.url().origin();
        for location in invalidation_candidates(&response.headers) {
            let Some(url) = std::str::from_utf8(location)
                .ok()
                .and_then(|location| request.current_url().join(location).ok())
            else {
                continue;
            };
            // "A cache MUST NOT trigger an invalidation under these conditions if
            // the origin (Section 4.3.1 of [HTTP]) of the URI to be invalidated
            // differs from that of the target URI."
            if url.origin() != origin {
                continue;
            }
            let key = CacheKey::from_url(url);
            if key != transaction.key {
                self.store.remove_key(&key).await;
            }
        }
    }
}

/// How a request may use the stored response selected for it.
#[derive(Clone, Copy, PartialEq)]
enum StoredResponseUse {
    /// Not at all.
    None,
    /// As it is.
    Serve,
    /// As it is, while it is revalidated in the background.
    ServeAndRevalidate,
    /// Only after it is validated.
    Validate,
}

/// Substeps 1 and 2 of step 8.26.2 of
/// <https://fetch.spec.whatwg.org/#concept-http-network-or-cache-fetch>.
fn stored_response_use(request: &Request, reuse: Reuse) -> StoredResponseUse {
    match (request.cache_mode, &request.mode) {
        (CacheMode::ForceCache, _) | (CacheMode::OnlyIfCached, RequestMode::SameOrigin) => {
            StoredResponseUse::Serve
        },
        (CacheMode::OnlyIfCached, _) | (CacheMode::NoStore, _) | (CacheMode::Reload, _) => {
            StoredResponseUse::None
        },
        // "no-cache" always makes a conditional request, also when a `Cache-Control`
        // of the request's own kept step 8.17 from adding `max-age=0`.
        (CacheMode::NoCache, _) => StoredResponseUse::Validate,
        (CacheMode::Default, _) => match reuse {
            Reuse::Fresh | Reuse::StaleAllowed => StoredResponseUse::Serve,
            // Only a request with a client revalidates in the background (substep
            // 1). Otherwise the response is validated, so that it does not stay
            // stale.
            Reuse::StaleWhileRevalidate if request.client.is_some() => {
                StoredResponseUse::ServeAndRevalidate
            },
            Reuse::StaleWhileRevalidate | Reuse::Validate => StoredResponseUse::Validate,
        },
    }
}

/// Whether a stored entry can answer `request` at all, and how.
fn suitability(request: &Request, meta: &EntryMeta, now: SystemTime) -> Option<Reuse> {
    if matches!(range::select(request, meta), RangeOutcome::Unsatisfiable) {
        return None;
    }
    meta.policy.evaluate(&request.method, &request.headers, now)
}

/// Which of two equally recent variants to use.
fn preference(reuse: Reuse) -> u8 {
    match reuse {
        Reuse::Validate => 0,
        Reuse::StaleWhileRevalidate => 1,
        Reuse::StaleAllowed => 2,
        Reuse::Fresh => 3,
    }
}

/// Whether a stored response is fresh, or needs validating before or after use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationStatus {
    /// Usable without any revalidation.
    Valid,
    /// Stale.
    Stale {
        /// Whether it may still be served immediately, leaving the caller to
        /// revalidate it in the background.
        revalidate_in_background: bool,
    },
}

#[cfg(feature = "test-util")]
impl HttpCache {
    /// Store a response and its body the way the network path would.
    /// Returns whether the response was storable.
    pub async fn store_for_test(
        &self,
        request: &Request,
        response: &Response,
        body: &[u8],
    ) -> bool {
        let transaction = self.new_transaction(request, watch::channel(Default::default()).1);
        let Some(meta) =
            self.entry_meta(&transaction, request, &response.status, &response.headers)
        else {
            return false;
        };
        let Ok(mut writer) = self.store.create(meta).await else {
            return false;
        };
        if !body.is_empty() {
            writer.push(bytes::Bytes::copy_from_slice(body));
        }
        writer.commit().await.is_ok()
    }

    /// What the cache would do with `request`, or `None` for a miss.
    pub async fn probe(&self, request: &Request) -> Option<ValidationStatus> {
        let mut transaction = self.new_transaction(request, watch::channel(Default::default()).1);
        let (_, _, reuse) = self.select_variant(&mut transaction, request).await?;
        Some(match stored_response_use(request, reuse) {
            StoredResponseUse::None => return None,
            StoredResponseUse::Serve => ValidationStatus::Valid,
            StoredResponseUse::ServeAndRevalidate => ValidationStatus::Stale {
                revalidate_in_background: true,
            },
            StoredResponseUse::Validate => ValidationStatus::Stale {
                revalidate_in_background: false,
            },
        })
    }

    /// The headers a stored response would be served with. Only for tests.
    pub async fn headers_for_test(&self, request: &Request) -> Option<HeaderMap> {
        let mut transaction = self.new_transaction(request, watch::channel(Default::default()).1);
        let (_, meta, _) = self.select_variant(&mut transaction, request).await?;
        Some(
            meta.policy
                .presented_fields(&meta.headers, SystemTime::now()),
        )
    }

    /// Read a stored response back as one buffer. Only for tests.
    pub async fn read_body_for_test(&self, request: &Request) -> Option<Vec<u8>> {
        let mut transaction = self.new_transaction(request, watch::channel(Default::default()).1);
        let (id, meta, _) = self.select_variant(&mut transaction, request).await?;
        let reader = self.store.open(id, &meta, None).await.ok()?;
        let mut stream = reader.into_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.ok()?);
        }
        Some(body)
    }

    /// Answer `request` from the cache as a fetch would, with the body it streams,
    /// or `None` for a miss. Only for tests.
    pub async fn serve_for_test(&self, request: &Request) -> Option<(Response, Vec<u8>)> {
        let mut transaction = self.new_transaction(request, watch::channel(Default::default()).1);
        let (id, meta, _) = self.select_variant(&mut transaction, request).await?;
        let headers = meta
            .policy
            .presented_fields(&meta.headers, SystemTime::now());
        let mut response = self
            .serve(request, id, &meta, headers, CacheState::Local)
            .await
            .ok()?;
        let ResponseBody::Stream(body) = response.body.take() else {
            return None;
        };
        let body = body.read_all().await.ok()?;
        Some((response, body.to_vec()))
    }

    /// Invalidate what an unsafe `request` that `response` answered invalidates.
    /// Only for tests.
    pub async fn invalidate_for_test(&self, request: &Request, response: &Response) {
        let transaction = self.new_transaction(request, watch::channel(Default::default()).1);
        self.invalidate(&transaction, request, response).await;
    }

    /// The number of responses stored for the URL of `request`. Only for tests.
    pub async fn stored_response_count(&self, request: &Request) -> usize {
        self.store.lookup(&CacheKey::new(request)).await.len()
    }

    /// How many fetches wait for the fetch that stores a fresh response to
    /// `request`, or `None` if no fetch does. Only for tests.
    pub fn waiting_fetch_count(&self, request: &Request) -> Option<usize> {
        self.inflight.waiter_count(&CacheKey::new(request))
    }

    /// The largest response the store accepts. Only for tests.
    pub fn max_entry_bytes_for_test(&self) -> u64 {
        self.store.max_entry_bytes()
    }

    /// Shorten how long a fetch waits for another fetch's write. Only for tests.
    pub fn set_wait_timeout_for_test(&self, timeout: std::time::Duration) {
        self.inflight.set_wait_timeout(timeout);
    }
}

/// Why a stored entry could not answer a request.
enum ServeError {
    /// The entry is no longer there.
    Gone,
    /// The entry is fine, but it cannot serve the requested range.
    RangeNotSatisfiable,
}

/// What a cache lookup produced.
#[derive(Default)]
pub(crate) struct CacheLookup {
    /// The response to serve, if the cache could answer the request.
    pub response: Option<Response>,
    /// Held while a `stale-while-revalidate` refresh should be started for this
    /// key; dropping it lets the next stale hit start another one.
    pub revalidate_in_background: Option<RevalidationGuard>,
}

/// State a fetch carries from consulting the cache to storing the network response.
pub(crate) struct CacheTransaction {
    key: CacheKey,
    /// Variants this request matched, with their stamps. A newly stored response
    /// replaces them.
    superseded: Vec<(EntryId, u64)>,
    /// The entry a 304 would freshen.
    revalidating: Option<(EntryId, EntryMeta)>,
    /// Held while this fetch is the one allowed to write `key`.
    writer: Option<InFlightWriter>,
    /// False when this request can never be cached, whatever comes back.
    storable: bool,
    /// The flow state of this fetch.
    producer: watch::Receiver<FetchFlowState>,
    /// When the request was last sent to the network.
    request_time: Option<SystemTime>,
    /// Whether the cache added the preconditions of an entry to the request.
    added_preconditions: bool,
}

impl CacheTransaction {
    fn cancelled(&self) -> bool {
        *self.producer.borrow() == FetchFlowState::Cancelled
    }

    /// Record that the request is being sent to the network now, which is the
    /// `request_time` of RFC 9111 section 4.2.3.
    pub(crate) fn mark_request_sent(&mut self) {
        self.request_time = Some(SystemTime::now());
    }

    fn request_time(&self) -> SystemTime {
        self.request_time.unwrap_or_else(SystemTime::now)
    }

    /// The request's header fields without the preconditions the cache added.
    fn presented_request_headers<'a>(&self, request: &'a Request) -> Cow<'a, HeaderMap> {
        if !self.added_preconditions {
            return Cow::Borrowed(&request.headers);
        }
        let mut headers = request.headers.clone();
        headers.remove(header::IF_NONE_MATCH);
        headers.remove(header::IF_MODIFIED_SINCE);
        Cow::Owned(headers)
    }
}

fn is_cacheable_request(request: &Request) -> bool {
    !pref!(network_http_cache_disabled) && request.method == Method::GET
}

/// Stream a stored entry to the fetch consumer, decoding it on the way.
fn spawn_entry_stream(reader: BodyReader, sender: BodySender) {
    let encoding = reader.content_encoding();
    tokio::spawn(async move {
        let mut decoder = Box::pin(Decoder::for_stream(reader.into_stream(), encoding));
        while let Some(item) = decoder.next().await {
            match item {
                Ok(chunk) => {
                    if !sender.send(chunk) {
                        return;
                    }
                },
                Err(error) => {
                    debug!("error reading a cached body: {error}");
                    sender.finish(BodyEnd::Failed(NetworkError::DecompressionError));
                    return;
                },
            }
        }
        sender.finish(BodyEnd::Complete);
    });
}
