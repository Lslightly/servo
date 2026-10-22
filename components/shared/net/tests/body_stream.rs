/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use bytes::Bytes;
use net_traits::NetworkError;
use net_traits::body_stream::{BodyEnd, BodyEvent, BodyReceiver, BodySender, body_channel};

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

fn next_chunk(receiver: &mut BodyReceiver) -> Bytes {
    match block_on(receiver.next()) {
        BodyEvent::Chunk(chunk) => chunk,
        BodyEvent::End(end) => panic!("expected a chunk, got the end {end:?}"),
    }
}

fn next_end(receiver: &mut BodyReceiver) -> BodyEnd {
    match block_on(receiver.next()) {
        BodyEvent::Chunk(chunk) => panic!("expected the end, got a chunk {chunk:?}"),
        BodyEvent::End(end) => end,
    }
}

#[test]
fn test_body_delivers_chunks_then_its_end() {
    let (sender, mut receiver) = body_channel(Some(6));
    assert_eq!(receiver.length_hint(), Some(6));
    assert!(sender.send(Bytes::from_static(b"abc")));
    assert!(sender.send(Bytes::from_static(b"def")));
    sender.finish(BodyEnd::Complete);

    assert_eq!(next_chunk(&mut receiver), &b"abc"[..]);
    assert_eq!(next_chunk(&mut receiver), &b"def"[..]);
    assert_eq!(next_end(&mut receiver), BodyEnd::Complete);
    assert_eq!(next_end(&mut receiver), BodyEnd::Complete);
}

#[test]
fn test_body_without_finish_fails() {
    let (sender, mut receiver) = body_channel(None);
    assert!(sender.send(Bytes::from_static(b"abc")));
    drop(sender);

    assert_eq!(next_chunk(&mut receiver), &b"abc"[..]);
    let end = next_end(&mut receiver);
    assert!(matches!(end, BodyEnd::Failed(_)));
    assert_eq!(next_end(&mut receiver), end);
}

#[test]
fn test_body_of_a_panicking_producer_fails() {
    let (sender, receiver) = body_channel(None);
    let producer = std::thread::spawn(move || {
        assert!(sender.send(Bytes::from_static(b"abc")));
        panic!("the producer failed");
    });
    assert!(producer.join().is_err());

    assert!(matches!(
        block_on(receiver.read_all()),
        Err(BodyEnd::Failed(_))
    ));
}

#[test]
fn test_send_reports_a_dropped_receiver() {
    let (sender, receiver) = body_channel(None);
    assert!(!sender.is_closed());
    drop(receiver);

    assert!(sender.is_closed());
    assert!(!sender.send(Bytes::from_static(b"abc")));
}

#[test]
fn test_body_of_a_panicking_producer_task_fails() {
    block_on(async {
        let (sender, receiver) = body_channel(None);
        let producer = tokio::spawn(async move {
            assert!(sender.send(Bytes::from_static(b"abc")));
            panic!("the producer failed");
        });
        assert!(producer.await.is_err());
        assert!(matches!(receiver.read_all().await, Err(BodyEnd::Failed(_))));
    });
}

#[test]
fn test_length_hint_is_capped() {
    let (_sender, receiver) = body_channel(Some(usize::MAX));
    assert!(receiver.length_hint().unwrap() < usize::MAX);
}

#[test]
fn test_body_ends_can_be_held_across_awaits() {
    fn assert_send_and_sync<T: Send + Sync>() {}
    assert_send_and_sync::<BodySender>();
    assert_send_and_sync::<BodyReceiver>();
}

#[test]
fn test_read_all_concatenates_a_complete_body() {
    let (sender, receiver) = body_channel(Some(1));
    assert!(sender.send(Bytes::from_static(b"abc")));
    assert!(sender.send(Bytes::new()));
    assert!(sender.send(Bytes::from_static(b"def")));
    sender.finish(BodyEnd::Complete);

    assert_eq!(block_on(receiver.read_all()).unwrap(), &b"abcdef"[..]);
}

#[test]
fn test_read_all_reports_how_an_incomplete_body_ended() {
    let (sender, receiver) = body_channel(None);
    assert!(sender.send(Bytes::from_static(b"abc")));
    sender.finish(BodyEnd::Failed(NetworkError::DecompressionError));
    assert_eq!(
        block_on(receiver.read_all()),
        Err(BodyEnd::Failed(NetworkError::DecompressionError))
    );

    let (sender, receiver) = body_channel(None);
    sender.finish(BodyEnd::Aborted);
    assert_eq!(block_on(receiver.read_all()), Err(BodyEnd::Aborted));
}

#[test]
fn test_body_is_read_from_another_thread() {
    let (sender, receiver) = body_channel(None);
    let producer = std::thread::spawn(move || {
        for _ in 0..100 {
            assert!(sender.send(Bytes::from_static(b"x")));
        }
        sender.finish(BodyEnd::Complete);
    });

    assert_eq!(block_on(receiver.read_all()).unwrap().len(), 100);
    producer.join().unwrap();
}
