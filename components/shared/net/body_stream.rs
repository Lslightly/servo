/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The [stream](https://fetch.spec.whatwg.org/#concept-body-stream) of a response body, from
//! the task that receives the body to the fetch that hands it over to its consumer.

use bytes::{Bytes, BytesMut};
use log::warn;
use servo_config::pref;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::NetworkError;

/// How a response body ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BodyEnd {
    /// The whole body was received.
    Complete,
    /// The fetch was aborted before the end of the body.
    ///
    /// <https://fetch.spec.whatwg.org/#concept-response-aborted>
    Aborted,
    /// The body could not be received or decoded to its end.
    Failed(NetworkError),
}

/// What a [`BodyReceiver`] yields: chunks, then how the body ended.
#[derive(Debug)]
pub enum BodyEvent {
    Chunk(Bytes),
    End(BodyEnd),
}

/// Create both ends of a response body stream. `length_hint` is how many bytes to preallocate
/// for the body, if a size can be guessed; it is capped at the `network_max_content_length`
/// preference and is never checked against the body.
pub fn body_channel(length_hint: Option<usize>) -> (BodySender, BodyReceiver) {
    let (sender, receiver) = unbounded_channel();
    let max_hint = pref!(network_max_content_length) as usize;
    (
        BodySender { sender },
        BodyReceiver {
            receiver,
            length_hint: length_hint.map(|hint| hint.min(max_hint)),
            end: None,
        },
    )
}

/// The producing end of a response body stream. A body has a single producer, so this is not
/// `Clone`, and the body ends when the producer is gone.
#[derive(Debug)]
pub struct BodySender {
    sender: UnboundedSender<BodyEvent>,
}

impl BodySender {
    /// Send the next chunk of the body. Returns `false` if nobody reads the body any more.
    #[must_use]
    pub fn send(&self, chunk: Bytes) -> bool {
        self.sender.send(BodyEvent::Chunk(chunk)).is_ok()
    }

    /// Whether the receiving end was dropped.
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    /// End the body.
    pub fn finish(self, end: BodyEnd) {
        let _ = self.sender.send(BodyEvent::End(end));
    }
}

/// The consuming end of a response body stream. If the producer goes away without calling
/// [`BodySender::finish`], for example because it panicked, the body ends with
/// [`BodyEnd::Failed`].
#[derive(Debug)]
pub struct BodyReceiver {
    receiver: UnboundedReceiver<BodyEvent>,
    length_hint: Option<usize>,
    end: Option<BodyEnd>,
}

impl BodyReceiver {
    pub fn length_hint(&self) -> Option<usize> {
        self.length_hint
    }

    /// The next chunk, or how the body ended. Once the body has ended, this keeps returning
    /// that end.
    pub async fn next(&mut self) -> BodyEvent {
        if let Some(end) = &self.end {
            return BodyEvent::End(end.clone());
        }
        let event = self.receiver.recv().await.unwrap_or_else(|| {
            warn!("A response body ended without being finished by its producer");
            BodyEvent::End(BodyEnd::Failed(NetworkError::ResourceLoadError(
                "The response body ended without being finished".into(),
            )))
        });
        if let BodyEvent::End(end) = &event {
            self.end = Some(end.clone());
        }
        event
    }

    /// Read the whole body.
    ///
    /// <https://fetch.spec.whatwg.org/#body-fully-read>
    pub async fn read_all(mut self) -> Result<Bytes, BodyEnd> {
        let mut bytes = BytesMut::with_capacity(self.length_hint.unwrap_or(0));
        loop {
            match self.next().await {
                BodyEvent::Chunk(chunk) => bytes.extend_from_slice(&chunk),
                BodyEvent::End(BodyEnd::Complete) => return Ok(bytes.freeze()),
                BodyEvent::End(end) => return Err(end),
            }
        }
    }
}
