/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! One dedicated thread per disk store, doing all of its blocking I/O.
//!
//! A dedicated thread rather than `tokio::fs` because a cross-thread hand-off
//! costs 28 to 45 us on the OHOS devices, several times a 64 KiB read: with one
//! thread and one hand-off per *batch* of syscalls the cost is amortised, file
//! descriptors stay warm, and operations on one file keep their order. Doing the
//! I/O inline on a tokio worker is not an option either, since a cold 64 KiB read
//! on eMMC takes 375 us and writeback throttling can stall a write for
//! milliseconds.

use std::thread;

use log::debug;
use servo_base::threadboost::{BoostAffinity, ThreadPriority, boost_thread};
use tokio::sync::{mpsc, oneshot};

/// How many operations may be queued before a caller waits. Deep enough that
/// several concurrent fetches do not serialise on handing work over.
const QUEUE_DEPTH: usize = 64;

type Job = Box<dyn FnOnce() + Send>;

/// A handle to a store's I/O thread.
pub(crate) struct IoThread {
    jobs: mpsc::Sender<Job>,
}

impl IoThread {
    pub(crate) fn spawn(name: String) -> IoThread {
        let (jobs, mut receiver) = mpsc::channel::<Job>(QUEUE_DEPTH);
        let spawned = thread::Builder::new().name(name).spawn(move || {
            // Default core placement made the hand-off four times more expensive
            // than the big cores of a phone, and the hand-off is what this thread
            // exists to amortise.
            boost_thread(ThreadPriority::Default, BoostAffinity::Boost);
            while let Some(job) = receiver.blocking_recv() {
                job();
            }
        });
        if let Err(error) = spawned {
            debug!("could not start the HTTP cache I/O thread: {error}");
        }
        IoThread { jobs }
    }

    /// Run `job` on the I/O thread. `None` means the thread is gone, which every
    /// caller must treat as a cache miss rather than an error.
    pub(crate) async fn run<R, F>(&self, job: F) -> Option<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let (reply, result) = oneshot::channel();
        let job: Job = Box::new(move || {
            let _ = reply.send(job());
        });
        self.jobs.send(job).await.ok()?;
        result.await.ok()
    }

    /// Queue `job` without waiting for it. Used for work whose result nothing
    /// needs, such as unlinking evicted entries and writing the index.
    ///
    /// A full queue must not cost the job: an unlink that is dropped leaves a file
    /// the next rescan believes in, which would resurrect an entry the cache has
    /// already decided to forget.
    pub(crate) fn detach<F>(&self, job: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let job: Job = Box::new(job);
        let Err(full) = self.jobs.try_send(job) else {
            return;
        };
        let mpsc::error::TrySendError::Full(job) = full else {
            debug!("the HTTP cache I/O thread is gone, dropping background work");
            return;
        };
        let jobs = self.jobs.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = jobs.send(job).await;
                });
            },
            // Off a runtime there is nothing to yield to, so wait for room here.
            Err(_) => {
                let _ = jobs.blocking_send(job);
            },
        }
    }
}
