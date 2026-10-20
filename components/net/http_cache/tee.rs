/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Copying a network body into the cache while it is delivered.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use hyper::body::{Body, Frame, SizeHint};
use log::debug;

use crate::connector::BoxedBody;
use crate::http_cache::inflight::InFlightWriter;
use crate::http_cache::store::EntryWriter;

/// What is written for one entry: its body, and the right to write its key.
struct Target {
    writer: EntryWriter,
    inflight: InFlightWriter,
}

/// Wraps a network body and copies every frame into an [`EntryWriter`] on its way
/// through. The copy is a refcount clone of the `Bytes`, so the bytes are not
/// duplicated, and the writer's bounded queue back-pressures the network.
///
/// This sits *below* the content decoder, so what reaches the cache is the body
/// exactly as it came off the wire, with its `Content-Encoding` intact. For the
/// same reason the end of this body does not publish the entry: a decoder stops
/// reading at the end of its coding, before the network body reports its end,
/// and a body that ends cleanly may still fail to decode. [`TeeCommit`] publishes
/// it once the decoded body has ended.
pub(crate) struct TeeBody {
    inner: BoxedBody,
    target: Arc<Mutex<Option<Target>>>,
    /// Bytes copied so far, and how many the store accepts for one entry.
    copied: u64,
    max_bytes: u64,
}

/// Publishes the entry a [`TeeBody`] writes. Dropping it without committing
/// aborts the entry.
pub(crate) struct TeeCommit {
    target: Arc<Mutex<Option<Target>>>,
}

impl TeeBody {
    pub(crate) fn new(
        inner: BoxedBody,
        writer: EntryWriter,
        inflight: InFlightWriter,
        max_bytes: u64,
    ) -> (Self, TeeCommit) {
        let target = Arc::new(Mutex::new(Some(Target { writer, inflight })));
        let commit = TeeCommit {
            target: target.clone(),
        };
        let body = Self {
            inner,
            target,
            copied: 0,
            max_bytes,
        };
        (body, commit)
    }
}

impl TeeCommit {
    /// The body was decoded to its end: publish the entry once the store has it all.
    pub(crate) fn commit(self) {
        let Some(Target { writer, inflight }) = self.target.lock().unwrap().take() else {
            return;
        };
        inflight.committing();
        tokio::spawn(async move {
            match writer.commit().await {
                Ok(id) => inflight.commit(id),
                Err(error) => {
                    debug!("cache entry was not committed: {error}");
                    inflight.abort();
                },
            }
        });
    }
}

impl Drop for TeeCommit {
    fn drop(&mut self) {
        // Dropping the writer aborts the entry, and dropping the in-flight handle
        // releases the waiters.
        self.target.lock().unwrap().take();
    }
}

impl Body for TeeBody {
    type Data = <BoxedBody as Body>::Data;
    type Error = <BoxedBody as Body>::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();

        // Do not pull from the network until the store can take another chunk.
        if let Some(target) = this.target.lock().unwrap().as_mut() &&
            target.writer.poll_ready(cx).is_pending()
        {
            return Poll::Pending;
        }

        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let mut target = this.target.lock().unwrap();
                    this.copied += data.len() as u64;
                    if this.copied > this.max_bytes {
                        // The store would decline the entry at commit time, so stop
                        // copying, and let the fetches that wait for it go now.
                        if let Some(target) = target.take() {
                            debug!("cache entry outgrew the per-entry cap");
                            target.inflight.abort();
                        }
                    } else if let Some(target) = target.as_mut() {
                        target.writer.push(data.clone());
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            },
            Poll::Ready(Some(Err(error))) => {
                // The body failed: drop the partial entry.
                this.target.lock().unwrap().take();
                Poll::Ready(Some(Err(error)))
            },
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
