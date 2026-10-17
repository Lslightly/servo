/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use malloc_size_of_derive::MallocSizeOf;
use servo_base::generic_channel::GenericCallback;

use crate::request::RequestId;
use crate::{CoreResourceThread, cancel_async_fetch, resume_async_fetch, suspend_async_fetch};

/// <https://fetch.spec.whatwg.org/#fetch-controller-state>
#[derive(Default, MallocSizeOf, PartialEq)]
pub enum FetchControllerState {
    #[default]
    Ongoing,
    Terminated,
    Aborted,
}

/// Fetch controller object. By default initialized to having a
/// request associated with it, which can be aborted or terminated.
/// Calling `ignore` will sever the relationship with the request,
/// meaning it cannot be cancelled through this controller from that point on.
/// <https://fetch.spec.whatwg.org/#fetch-controller>
#[derive(Default, MallocSizeOf)]
pub struct FetchController {
    request_id: Option<RequestId>,
    core_resource_thread: Option<CoreResourceThread>,
    keep_alive: bool,
    /// <https://fetch.spec.whatwg.org/#fetch-controller-state>
    pub state: FetchControllerState,
}

impl FetchController {
    /// Create a FetchController associated with a request,
    /// and a particular(public vs private) resource thread.
    pub fn new(
        request_id: RequestId,
        keep_alive: bool,
        core_resource_thread: CoreResourceThread,
    ) -> Self {
        Self {
            request_id: Some(request_id),
            core_resource_thread: Some(core_resource_thread),
            keep_alive,
            state: Default::default(),
        }
    }

    pub fn keep_alive(&self) -> bool {
        self.keep_alive
    }

    fn cancel(&mut self) {
        if let Some(request_id) = self.request_id.take() {
            // stop trying to make fetch happen
            // it's not going to happen

            if let Some(ref core_resource_thread) = self.core_resource_thread {
                // No error handling here. Cancellation is a courtesy call,
                // we don't actually care if the other side heard.
                cancel_async_fetch(vec![request_id], core_resource_thread);
            }
        }
    }

    /// Ask to stop reading the response body until [`Self::resume`] is called, so that flow
    /// control holds back the server. Data that was already received is still delivered.
    ///
    /// This is advisory, so the body may keep arriving: net ignores it when the body is not
    /// being read from the network (cache hits, other schemes, after the end), and when two
    /// fetches to the origin of the request are suspended already.
    ///
    /// A fetch that stays suspended for longer than the `network_fetch_suspend_timeout_ms` pref
    /// ends with [`NetworkError::ConnectionFailure`](crate::NetworkError::ConnectionFailure),
    /// which cannot be told apart from a lost connection. A consumer that suspended a fetch
    /// should restart it after such an error, for example with a Range request for the rest.
    ///
    /// `reply`, if any, receives whether net suspended the fetch: false if net ignored the
    /// suspension. Nothing is sent if this controller no longer has a request.
    pub fn suspend(&self, reply: Option<GenericCallback<bool>>) {
        if let (Some(request_id), Some(core_resource_thread)) =
            (self.request_id, &self.core_resource_thread)
        {
            suspend_async_fetch(request_id, reply, core_resource_thread);
        }
    }

    /// Continue reading a response body that [`Self::suspend`] stopped.
    pub fn resume(&self) {
        if let (Some(request_id), Some(core_resource_thread)) =
            (self.request_id, &self.core_resource_thread)
        {
            resume_async_fetch(request_id, core_resource_thread);
        }
    }

    /// Use this if you don't want it to send a cancellation request
    /// on drop (e.g. if the fetch completes)
    pub fn ignore(&mut self) {
        let _ = self.request_id.take();
    }

    /// <https://fetch.spec.whatwg.org/#fetch-controller-abort>
    pub fn abort(&mut self) {
        // Step 1. Set controller’s state to "aborted".
        self.state = FetchControllerState::Aborted;

        self.cancel();
    }

    /// <https://fetch.spec.whatwg.org/#fetch-controller-terminate>
    pub fn terminate(&mut self) {
        // > To terminate a fetch controller controller,
        // > set controller’s state to "terminated".
        self.state = FetchControllerState::Terminated;

        self.cancel();
    }
}
