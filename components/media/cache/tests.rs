/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;

use crate::cache::Settings;
use crate::clock::FakeClock;
use crate::store::BLOCK_SIZE;
use crate::*;

const MIB: u64 = 1024 * 1024;
const KIB: u64 = 1024;

fn pattern(seed: u64, pos: u64) -> u8 {
    ((pos + seed) % 251) as u8
}

#[derive(Clone)]
struct ServerConfig {
    /// The bytes the server has.
    length: u64,
    /// The length it announces, `None` for an unknown length.
    announced_length: Option<u64>,
    ranges: bool,
    seed: u64,
    validator: Option<Validator>,
    origin: Option<String>,
    cors_tainted: bool,
    /// Answers a range request after the start with a 206 from this much earlier, or later
    /// if negative.
    answer_shift: i64,
    chunk: usize,
    decline_suspend: bool,
    /// Ends each fetch with a network error after this many bytes.
    fail_after: Option<u64>,
    /// Ends each fetch without an error after this many bytes.
    close_after: Option<u64>,
}

impl ServerConfig {
    fn new(length: u64) -> Self {
        Self {
            length,
            announced_length: Some(length),
            ranges: true,
            seed: 0,
            validator: Some(Validator::ETag("\"v1\"".into())),
            origin: Some("https://example.com".into()),
            cors_tainted: false,
            answer_shift: 0,
            chunk: 16 * KIB as usize,
            decline_suspend: false,
            fail_after: None,
            close_after: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum FakeState {
    Running,
    Suspended,
    Cancelled,
    Ended,
}

struct FakeFetch {
    request: FetchRequest,
    sink: Arc<dyn FetchSink>,
    config: ServerConfig,
    state: FakeState,
    responded: bool,
    pos: u64,
    sent: u64,
    suspends: u32,
    resumes: u32,
}

#[derive(Default)]
struct FakeServer {
    config: Mutex<Option<ServerConfig>>,
    fetches: Mutex<Vec<FakeFetch>>,
}

enum Step {
    Response(ResponseInfo),
    Data(Bytes),
    End(Result<(), FetchError>),
}

impl FakeServer {
    fn new(config: ServerConfig) -> Arc<Self> {
        let server = Arc::new(FakeServer::default());
        server.set_config(config);
        server
    }

    fn set_config(&self, config: ServerConfig) {
        *self.config.lock() = Some(config);
    }

    fn update_config(&self, change: impl FnOnce(&mut ServerConfig)) {
        change(self.config.lock().as_mut().unwrap());
    }

    fn fetcher(self: &Arc<Self>) -> Arc<dyn Fetcher> {
        Arc::new(FakeFetcher(self.clone()))
    }

    fn requests(&self) -> Vec<FetchRequest> {
        self.fetches
            .lock()
            .iter()
            .map(|fetch| fetch.request.clone())
            .collect()
    }

    fn state(&self, index: usize) -> FakeState {
        self.fetches.lock()[index].state
    }

    fn suspends(&self, index: usize) -> u32 {
        self.fetches.lock()[index].suspends
    }

    fn resumes(&self, index: usize) -> u32 {
        self.fetches.lock()[index].resumes
    }

    fn sent(&self, index: usize) -> u64 {
        self.fetches.lock()[index].sent
    }

    /// Makes one running fetch take one step. Calls the sink without the server's lock, as
    /// the sink may call back into the handle.
    fn step(&self) -> bool {
        self.step_matching(|_| true)
    }

    fn step_matching(&self, matches: impl Fn(usize) -> bool) -> bool {
        let (sink, step) = {
            let mut fetches = self.fetches.lock();
            let Some((_, fetch)) = fetches
                .iter_mut()
                .enumerate()
                .find(|(index, fetch)| fetch.state == FakeState::Running && matches(*index))
            else {
                return false;
            };
            let config = fetch.config.clone();
            let step = if !fetch.responded {
                fetch.responded = true;
                let start = fetch.request.start;
                if config.ranges &&
                    config
                        .announced_length
                        .is_some_and(|length| start >= length) ||
                    config.ranges && start >= config.length && start > 0
                {
                    fetch.state = FakeState::Ended;
                    Step::End(Err(FetchError::RangeNotSatisfiable))
                } else {
                    fetch.pos = match config.ranges {
                        true if start > 0 => start.saturating_add_signed(-config.answer_shift),
                        true => start,
                        false => 0,
                    };
                    Step::Response(ResponseInfo {
                        partial: config.ranges,
                        start: fetch.pos,
                        total_length: config.announced_length,
                        validator: config.validator.clone(),
                        encoded: false,
                        cors_tainted: config.cors_tainted,
                        origin: config.origin.clone(),
                    })
                }
            } else if config.fail_after.is_some_and(|limit| fetch.sent >= limit) {
                fetch.state = FakeState::Ended;
                Step::End(Err(FetchError::Network))
            } else if fetch.pos >= config.length ||
                config.close_after.is_some_and(|limit| fetch.sent >= limit)
            {
                fetch.state = FakeState::Ended;
                Step::End(Ok(()))
            } else {
                let mut count = (config.chunk as u64).min(config.length - fetch.pos);
                for limit in [config.fail_after, config.close_after]
                    .into_iter()
                    .flatten()
                {
                    count = count.min(limit - fetch.sent);
                }
                let data: Vec<u8> = (fetch.pos..fetch.pos + count)
                    .map(|pos| pattern(config.seed, pos))
                    .collect();
                fetch.pos += count;
                fetch.sent += count;
                Step::Data(Bytes::from(data))
            };
            (fetch.sink.clone(), step)
        };
        match step {
            Step::Response(info) => sink.response(info),
            Step::Data(data) => sink.data(data),
            Step::End(result) => sink.end(result),
        }
        true
    }

    /// Makes fetch `index` take one step, if it is running.
    fn step_fetch(&self, index: usize) -> bool {
        self.step_matching(|other| other == index)
    }

    /// Runs the fetches until all are suspended, cancelled or done.
    fn pump(&self) {
        let mut steps = 0;
        while self.step() {
            steps += 1;
            assert!(steps < 100_000, "the fetches never stop");
        }
    }
}

struct FakeFetcher(Arc<FakeServer>);

impl Fetcher for FakeFetcher {
    fn start(&self, request: FetchRequest, sink: Arc<dyn FetchSink>) -> Box<dyn FetchHandle> {
        let config = self.0.config.lock().clone().unwrap();
        let mut fetches = self.0.fetches.lock();
        fetches.push(FakeFetch {
            request,
            sink,
            config,
            state: FakeState::Running,
            responded: false,
            pos: 0,
            sent: 0,
            suspends: 0,
            resumes: 0,
        });
        Box::new(FakeHandle {
            server: self.0.clone(),
            index: fetches.len() - 1,
        })
    }
}

struct FakeHandle {
    server: Arc<FakeServer>,
    index: usize,
}

impl FetchHandle for FakeHandle {
    fn suspend(&self) {
        let (sink, honoured) = {
            let mut fetches = self.server.fetches.lock();
            let fetch = &mut fetches[self.index];
            fetch.suspends += 1;
            let honoured = !fetch.config.decline_suspend && fetch.state == FakeState::Running;
            if honoured {
                fetch.state = FakeState::Suspended;
            }
            (fetch.sink.clone(), honoured)
        };
        sink.suspended(honoured);
    }

    fn resume(&self) {
        let mut fetches = self.server.fetches.lock();
        let fetch = &mut fetches[self.index];
        fetch.resumes += 1;
        if fetch.state == FakeState::Suspended {
            fetch.state = FakeState::Running;
        }
    }

    fn cancel(&self) {
        let mut fetches = self.server.fetches.lock();
        let fetch = &mut fetches[self.index];
        if matches!(fetch.state, FakeState::Running | FakeState::Suspended) {
            fetch.state = FakeState::Cancelled;
        }
    }
}

impl Drop for FakeHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn settings() -> Settings {
    Settings {
        file_budget: 64 * MIB,
        memory_resource_limit: 8 * MIB,
        memory_fallback: 32 * MIB,
        readahead: Duration::from_secs(30),
        resume: Duration::from_secs(10),
        suspend_timeout: Duration::from_secs(30),
        reuse: Duration::from_secs(300),
        directory: None,
    }
}

fn key(name: &str) -> ResourceKey {
    ResourceKey {
        url: format!("https://example.com/{name}"),
        cors_mode: CorsMode::NoCors,
        origin: "https://example.com".into(),
    }
}

struct Setup {
    clock: Arc<FakeClock>,
    cache: MediaCache,
    _directory: tempfile::TempDir,
}

fn setup(mut settings: Settings) -> Setup {
    let clock = Arc::new(FakeClock::default());
    let directory = tempfile::tempdir().unwrap();
    settings
        .directory
        .get_or_insert_with(|| directory.path().to_owned());
    let cache = MediaCache::with_clock(settings, clock.clone());
    Setup {
        clock,
        cache,
        _directory: directory,
    }
}

impl Setup {
    fn advance(&self, seconds: u64) {
        self.clock.advance(Duration::from_secs(seconds));
    }

    fn with_resource<R>(
        &self,
        key: &ResourceKey,
        f: impl FnOnce(&crate::resource::Resource) -> R,
    ) -> R {
        let state = self.cache.inner.state.lock();
        f(state.resource(key).expect("resource exists"))
    }

    fn block_indices(&self, key: &ResourceKey) -> Vec<u64> {
        self.with_resource(key, |res| res.blocks.keys().copied().collect())
    }
}

/// Reads `len` bytes at `pos`, pumping the server while the data is pending, and checks them.
fn read_expect(server: &FakeServer, reader: &MediaReader, pos: u64, len: u64, seed: u64) {
    let mut buffer = vec![0; 100 * KIB as usize];
    let mut at = pos;
    let mut steps = 0;
    while at < pos + len {
        steps += 1;
        assert!(steps < 100_000, "no progress at {at}");
        let want = ((pos + len - at) as usize).min(buffer.len());
        match reader.read_at(at, &mut buffer[..want]) {
            ReadResult::Data(count) => {
                for (offset, byte) in buffer[..count].iter().enumerate() {
                    assert_eq!(
                        *byte,
                        pattern(seed, at + offset as u64),
                        "byte at {}",
                        at + offset as u64
                    );
                }
                at += count as u64;
            },
            ReadResult::Pending => assert!(server.step(), "pending at {at} with nothing to fetch"),
            other => panic!("read at {at}: {other:?}"),
        }
    }
}

/// Reads at `pos`, stepping the server while the read is pending, at most `steps` times.
fn read_settled(server: &FakeServer, reader: &MediaReader, pos: u64, steps: usize) -> ReadResult {
    for _ in 0..steps {
        match read_once(reader, pos) {
            ReadResult::Pending => {
                if !server.step() {
                    return ReadResult::Pending;
                }
            },
            other => return other,
        }
    }
    ReadResult::Pending
}

fn read_once(reader: &MediaReader, pos: u64) -> ReadResult {
    let mut buffer = vec![0; 4096];
    reader.read_at(pos, &mut buffer)
}

fn events(reader: &MediaReader) -> Arc<Mutex<Vec<ReaderEvent>>> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    reader.set_observer(Box::new(move |event| sink.lock().push(event)));
    events
}

#[test]
fn sequential_read() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(20 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    let events = events(&reader);
    read_expect(&server, &reader, 0, 20 * MIB, 0);
    server.pump();
    assert_eq!(read_once(&reader, 20 * MIB), ReadResult::EndOfStream);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].start, 0);
    assert_eq!(requests[0].if_range, None);
    assert_eq!(reader.length(), Some(20 * MIB));
    assert!(reader.seekable());
    let events = events.lock();
    assert_eq!(
        events[0],
        ReaderEvent::Metadata {
            length: Some(20 * MIB),
            seekable: true,
            cors_tainted: false
        }
    );
    assert!(events.contains(&ReaderEvent::Progress));
    assert_eq!(events.last(), Some(&ReaderEvent::Complete));
}

#[test]
fn two_readers_share_one_fetch() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(12 * MIB));
    let first = setup.cache.open(key("a"), server.fetcher());
    let second = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &first, 0, 3 * MIB, 0);
    read_expect(&server, &second, 0, 12 * MIB, 0);
    read_expect(&server, &first, 3 * MIB, 9 * MIB, 0);
    assert_eq!(server.requests().len(), 1);
    // Another key is another resource.
    let _third = setup.cache.open(key("b"), server.fetcher());
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn far_read_starts_a_gap_fetch_and_keeps_the_main_fetch_suspended() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
    server.pump();
    assert_eq!(server.state(0), FakeState::Suspended);

    assert_eq!(read_once(&reader, 30 * MIB + 100), ReadResult::Pending);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].start, 30 * MIB);
    assert_eq!(requests[1].if_range, Some(Validator::ETag("\"v1\"".into())));
    read_expect(&server, &reader, 30 * MIB + 100, 2 * MIB, 0);
    assert_eq!(server.state(0), FakeState::Suspended);
    assert_eq!(server.resumes(0), 0);

    // A read within 1 MiB ahead of the gap fetch waits for it instead of a third fetch.
    server.pump();
    let gap_end = 30 * MIB + server.sent(1);
    assert_eq!(read_once(&reader, gap_end + 512 * KIB), ReadResult::Pending);
    assert_eq!(server.requests().len(), 2);
    assert_eq!(server.state(1), FakeState::Running);

    // A third need cancels the fetch furthest from the reader.
    assert_eq!(read_once(&reader, 70 * MIB), ReadResult::Pending);
    assert_eq!(server.requests().len(), 3);
    assert_eq!(server.requests()[2].start, 70 * MIB);
    assert_eq!(server.state(0), FakeState::Cancelled);
    assert_ne!(server.state(1), FakeState::Cancelled);
}

/// Reads `len` bytes at `pos`, stepping only fetch `index` while the data is pending.
fn read_from_fetch(server: &FakeServer, reader: &MediaReader, pos: u64, len: u64, index: usize) {
    let mut buffer = vec![0; len as usize];
    let mut at = pos;
    while at < pos + len {
        match reader.read_at(at, &mut buffer[(at - pos) as usize..]) {
            ReadResult::Data(count) => at += count as u64,
            ReadResult::Pending => assert!(server.step_fetch(index), "fetch {index} stopped"),
            other => panic!("read at {at}: {other:?}"),
        }
    }
}

#[test]
fn alternating_reads_keep_both_fetches_running() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    // A file whose audio and video are far apart: the demuxer reads at two places in turn.
    for round in 0..10 {
        read_from_fetch(&server, &reader, round * 64 * KIB, 64 * KIB, 0);
        read_from_fetch(&server, &reader, 30 * MIB + round * 64 * KIB, 64 * KIB, 1);
    }
    assert_eq!(server.requests().len(), 2);
    assert_eq!(server.suspends(0), 0);
    assert_eq!(server.suspends(1), 0);
    // Once the reader stays at one place for a while, the other fetch is suspended.
    setup.advance(3);
    read_from_fetch(&server, &reader, 30 * MIB + 640 * KIB, 64 * KIB, 1);
    assert_eq!(server.suspends(0), 1);
}

#[test]
fn recent_positions_do_not_start_fetches() {
    let setup = setup(settings());
    let mut config = ServerConfig::new(100 * MIB);
    config.close_after = Some(2 * MIB);
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_from_fetch(&server, &reader, 0, MIB, 0);
    // A seek, then the main fetch ends early. The position that the reader left must not
    // start a request.
    assert_eq!(read_once(&reader, 30 * MIB), ReadResult::Pending);
    while server.step_fetch(0) {}
    assert_eq!(server.state(0), FakeState::Ended);
    assert_eq!(server.requests().len(), 2);
    // Back there, it does.
    read_from_fetch(&server, &reader, MIB, MIB, 2);
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn three_far_apart_readers_share_two_fetches() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let readers: Vec<MediaReader> = (0..3)
        .map(|_| setup.cache.open(key("a"), server.fetcher()))
        .collect();
    let starts = [0, 30 * MIB, 60 * MIB];
    for round in 0..8 {
        for (reader, start) in readers.iter().zip(starts) {
            read_expect(&server, reader, start + round * 256 * KIB, 256 * KIB, 0);
        }
        let live = server
            .fetches
            .lock()
            .iter()
            .filter(|fetch| matches!(fetch.state, FakeState::Running | FakeState::Suspended))
            .count();
        assert!(live <= 2, "{live} fetches");
    }
    let requests = server.requests().len();
    assert!(requests <= 3 + 3 * 8, "{requests} requests");
}

#[test]
fn probe_at_the_end_costs_one_small_request() {
    let setup = setup(settings());
    let length = 40 * MIB + 1000;
    let server = FakeServer::new(ServerConfig::new(length));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, 256 * KIB, 0);
    // The demuxer looks for the index at the end.
    read_expect(&server, &reader, length - 100 * KIB, 100 * KIB, 0);
    assert_eq!(read_once(&reader, length), ReadResult::EndOfStream);
    server.pump();
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].start,
        (length - 100 * KIB) / BLOCK_SIZE * BLOCK_SIZE
    );
    assert_eq!(server.state(1), FakeState::Ended);
    assert_ne!(server.state(0), FakeState::Cancelled);
    // Playback from the start continues with the main fetch.
    read_expect(&server, &reader, 256 * KIB, 20 * MIB, 0);
    assert_eq!(server.requests().len(), 2);
    assert!(server.resumes(0) >= 1);
}

#[test]
fn suspends_at_the_target_and_resumes_below_the_threshold() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(40 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    let events = events(&reader);
    server.pump();
    assert_eq!(server.state(0), FakeState::Suspended);
    let sent = server.sent(0);
    assert!(
        (8 * MIB..8 * MIB + BLOCK_SIZE + 16 * KIB).contains(&sent),
        "{sent}"
    );
    assert!(events.lock().contains(&ReaderEvent::DownloadSuspended));

    // 2 MiB of read-ahead left: still suspended.
    read_expect(&server, &reader, 0, sent - 2 * MIB - BLOCK_SIZE, 0);
    assert_eq!(server.state(0), FakeState::Suspended);
    read_expect(
        &server,
        &reader,
        sent - 2 * MIB - BLOCK_SIZE,
        2 * BLOCK_SIZE,
        0,
    );
    assert_eq!(server.state(0), FakeState::Running);
    assert_eq!(events.lock().last(), Some(&ReaderEvent::DownloadResumed));
    server.pump();
    assert_eq!(server.state(0), FakeState::Suspended);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn time_based_thresholds_follow_the_rate() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(40 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    // 100 kB/s: 3 MB ahead, resume below 1 MB.
    reader.set_activity(ReaderActivity::Playing {
        bytes_per_second: Some(100_000.),
    });
    server.pump();
    assert_eq!(server.state(0), FakeState::Suspended);
    let sent = server.sent(0);
    assert!(
        (3_000_000..3_000_000 + BLOCK_SIZE + 16 * KIB).contains(&sent),
        "{sent}"
    );
    read_expect(&server, &reader, 0, sent - 1_000_000 - BLOCK_SIZE, 0);
    assert_eq!(server.state(0), FakeState::Suspended);
    read_expect(
        &server,
        &reader,
        sent - 1_000_000 - BLOCK_SIZE,
        2 * BLOCK_SIZE,
        0,
    );
    assert_eq!(server.state(0), FakeState::Running);

    // Paused, the reader keeps its target, then the fetch suspends.
    reader.set_activity(ReaderActivity::Paused {
        bytes_per_second: Some(100_000.),
    });
    server.pump();
    assert_eq!(server.state(0), FakeState::Suspended);
    let sent = server.sent(0);
    assert!(sent < 5 * MIB, "{sent}");
}

#[test]
fn a_paused_reader_loads_its_rate_times_the_read_ahead() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    // Paused from the start, at 100 kB/s: 3 MB ahead, resume below 1 MB.
    reader.set_activity(ReaderActivity::Paused {
        bytes_per_second: Some(100_000.),
    });
    server.pump();
    let sent = server.sent(0);
    assert!(
        (3_000_000..3_000_000 + BLOCK_SIZE + 16 * KIB).contains(&sent),
        "{sent}"
    );
    read_expect(&server, &reader, 0, sent - 1_000_000 - BLOCK_SIZE, 0);
    assert_eq!(server.state(0), FakeState::Suspended);
    read_expect(
        &server,
        &reader,
        sent - 1_000_000 - BLOCK_SIZE,
        2 * BLOCK_SIZE,
        0,
    );
    assert_eq!(server.state(0), FakeState::Running);
}

#[test]
fn an_idle_reader_loads_nothing_ahead() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
    reader.set_activity(ReaderActivity::Idle);
    server.pump();
    let sent = server.sent(0);
    assert!(sent < MIB + BLOCK_SIZE + 16 * KIB, "{sent}");
    assert_eq!(server.state(0), FakeState::Suspended);
    setup.advance(31);
    assert_eq!(server.state(0), FakeState::Cancelled);
    // Its blocks stay.
    assert_eq!(setup.block_indices(&key("a")).first(), Some(&0));
}

#[test]
fn a_pending_read_of_an_idle_reader_fetches_the_data() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
    reader.set_activity(ReaderActivity::Idle);
    server.pump();
    setup.advance(31);
    read_expect(&server, &reader, 50 * MIB, 1000, 0);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].start, 50 * MIB);
    // Only what was read.
    server.pump();
    assert!(server.sent(1) < 2 * BLOCK_SIZE, "{}", server.sent(1));
}

#[test]
fn metadata_activity_stops_early() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(40 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    reader.set_activity(ReaderActivity::Metadata);
    server.pump();
    assert_eq!(server.state(0), FakeState::Suspended);
    let sent = server.sent(0);
    assert!((MIB..MIB + BLOCK_SIZE + 16 * KIB).contains(&sent), "{sent}");
}

#[test]
fn declined_suspend_cancels_and_restarts_at_the_first_missing_byte() {
    let setup = setup(settings());
    let mut config = ServerConfig::new(40 * MIB);
    config.decline_suspend = true;
    config.chunk = 10_000;
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("a"), server.fetcher());
    server.pump();
    assert_eq!(server.suspends(0), 1);
    assert_eq!(server.state(0), FakeState::Cancelled);
    let sent = server.sent(0);
    assert_eq!(server.requests().len(), 1);

    read_expect(&server, &reader, 0, sent - 2 * MIB - BLOCK_SIZE, 0);
    assert_eq!(server.requests().len(), 1);
    read_expect(
        &server,
        &reader,
        sent - 2 * MIB - BLOCK_SIZE,
        2 * MIB + 2 * BLOCK_SIZE,
        0,
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].start, sent);
    assert_eq!(requests[1].if_range, Some(Validator::ETag("\"v1\"".into())));
    // The partial block of the cancelled fetch was continued correctly.
    read_expect(&server, &reader, 0, 16 * MIB, 0);
}

#[test]
fn declined_suspend_without_range_support_keeps_loading_up_to_half_the_budget() {
    let mut settings = settings();
    settings.file_budget = 16 * MIB;
    let setup = setup(settings);
    let mut config = ServerConfig::new(40 * MIB);
    config.ranges = false;
    config.decline_suspend = true;
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("a"), server.fetcher());
    // 3 MB ahead, then a suspend, which net declines.
    reader.set_activity(ReaderActivity::Playing {
        bytes_per_second: Some(100_000.),
    });
    server.pump();
    // The fetch kept loading up to half the budget, then tried again, and was cancelled.
    assert_eq!(server.suspends(0), 2);
    assert_eq!(server.state(0), FakeState::Cancelled);
    let sent = server.sent(0);
    assert!(
        (8 * MIB..8 * MIB + BLOCK_SIZE + 16 * KIB).contains(&sent),
        "{sent}"
    );
    // What it loaded plays without another request.
    read_expect(&server, &reader, 0, 6 * MIB, 0);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn resources_without_range_support_share_the_half_budget() {
    let mut settings = settings();
    settings.file_budget = 16 * MIB;
    let setup = setup(settings);
    let mut config = ServerConfig::new(40 * MIB);
    config.ranges = false;
    config.decline_suspend = true;
    let servers = [FakeServer::new(config.clone()), FakeServer::new(config)];
    let readers: Vec<MediaReader> = servers
        .iter()
        .enumerate()
        .map(|(index, server)| {
            let reader = setup.cache.open(key(&index.to_string()), server.fetcher());
            reader.set_activity(ReaderActivity::Playing {
                bytes_per_second: Some(100_000.),
            });
            reader
        })
        .collect();
    for server in &servers {
        server.step();
    }
    for server in &servers {
        server.pump();
    }
    // Each keeps loading up to a quarter of the budget, so neither evicts the other.
    for (index, server) in servers.iter().enumerate() {
        let sent = server.sent(0);
        assert!(
            (4 * MIB..4 * MIB + BLOCK_SIZE + 16 * KIB).contains(&sent),
            "{sent}"
        );
        assert_eq!(
            setup.block_indices(&key(&index.to_string())).len() as u64,
            sent.div_ceil(BLOCK_SIZE)
        );
    }
    drop(readers);
}

#[test]
fn suspension_timeout_cancels_seekable_fetches_only() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(40 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    server.pump();
    assert_eq!(server.state(0), FakeState::Suspended);
    setup.advance(29);
    assert_eq!(server.state(0), FakeState::Suspended);
    setup.advance(2);
    assert_eq!(server.state(0), FakeState::Cancelled);
    // Playback that needs more restarts at the first missing byte.
    let sent = server.sent(0);
    read_expect(&server, &reader, 0, sent + MIB, 0);
    assert_eq!(server.requests()[1].start, sent);

    let mut config = ServerConfig::new(40 * MIB);
    config.ranges = false;
    let server = FakeServer::new(config);
    let _reader = setup.cache.open(key("b"), server.fetcher());
    server.pump();
    assert_eq!(server.state(0), FakeState::Suspended);
    setup.advance(120);
    assert_eq!(server.state(0), FakeState::Suspended);
}

#[test]
fn without_range_support() {
    let mut settings = settings();
    settings.file_budget = 16 * MIB;
    let setup = setup(settings);
    let mut config = ServerConfig::new(40 * MIB);
    config.ranges = false;
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, 6 * MIB, 0);
    assert!(!reader.seekable());

    // A read ahead waits for the one fetch, which is not cancelled.
    read_expect(&server, &reader, 9 * MIB, MIB, 0);
    assert_eq!(server.requests().len(), 1);
    assert_ne!(server.state(0), FakeState::Cancelled);

    // A read behind it within the cache is served from the cache.
    read_expect(&server, &reader, 2 * MIB, MIB, 0);
    assert_eq!(server.requests().len(), 1);

    // A read behind it outside the cache fetches from the start again and discards the bytes
    // before the read.
    read_expect(&server, &reader, 6 * MIB, MIB, 0);
    setup.cache.trim(0);
    assert!(!setup.block_indices(&key("a")).contains(&(MIB / BLOCK_SIZE)));
    read_expect(&server, &reader, MIB + 100, 1000, 0);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(server.state(0), FakeState::Cancelled);
    let blocks = setup.block_indices(&key("a"));
    assert!(blocks.contains(&(MIB / BLOCK_SIZE)));
    assert!(!blocks.contains(&(MIB / BLOCK_SIZE - 1)), "{blocks:?}");
}

#[test]
fn restart_without_range_support_that_only_discards_is_a_network_error() {
    let mut settings = settings();
    settings.file_budget = 16 * MIB;
    let setup = setup(settings);
    let mut config = ServerConfig::new(40 * MIB);
    config.ranges = false;
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, 6 * MIB, 0);
    setup.cache.trim(0);
    // Restarts from the start get cut before they reach data that is kept.
    server.update_config(|config| config.close_after = Some(512 * KIB));
    assert_eq!(
        read_settled(&server, &reader, 2 * MIB + 100, 1000),
        ReadResult::Error(ReaderError::Network)
    );
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn restart_without_range_support_after_an_early_end() {
    let setup = setup(settings());
    let mut config = ServerConfig::new(20 * MIB);
    config.ranges = false;
    config.fail_after = Some(3 * MIB + 1234);
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("a"), server.fetcher());
    server.update_config(|config| config.fail_after = None);
    read_expect(&server, &reader, 0, 20 * MIB, 0);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].start, 3 * MIB + 1234);
}

#[test]
fn unknown_length() {
    let setup = setup(settings());
    let mut config = ServerConfig::new(10 * MIB + 5);
    config.announced_length = None;
    config.ranges = false;
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("a"), server.fetcher());
    let events = events(&reader);
    read_expect(&server, &reader, 0, MIB, 0);
    assert_eq!(reader.length(), None);
    assert!(!reader.seekable());
    read_expect(&server, &reader, MIB, 9 * MIB + 5, 0);
    loop {
        match read_once(&reader, 10 * MIB + 5) {
            ReadResult::EndOfStream => break,
            ReadResult::Pending => assert!(server.step()),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(reader.length(), Some(10 * MIB + 5));
    assert_eq!(server.requests().len(), 1);
    assert!(events.lock().contains(&ReaderEvent::Metadata {
        length: Some(10 * MIB + 5),
        seekable: false,
        cors_tainted: false
    }));
}

#[test]
fn changed_validator_is_a_resource_change() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(40 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    let events = events(&reader);
    read_expect(&server, &reader, 0, MIB, 0);
    server.update_config(|config| {
        config.validator = Some(Validator::ETag("\"v2\"".into()));
        config.seed = 7;
    });
    assert_eq!(read_once(&reader, 30 * MIB), ReadResult::Pending);
    let waked = Arc::new(AtomicUsize::new(0));
    let counter = waked.clone();
    reader.set_waker(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    server.pump();
    assert_eq!(waked.load(Ordering::SeqCst), 1);
    assert_eq!(
        read_once(&reader, 30 * MIB),
        ReadResult::Error(ReaderError::ResourceChanged)
    );
    assert_eq!(
        read_once(&reader, 0),
        ReadResult::Error(ReaderError::ResourceChanged)
    );
    assert_eq!(
        events.lock().last(),
        Some(&ReaderEvent::Error(ReaderError::ResourceChanged))
    );
    assert_eq!(server.state(0), FakeState::Cancelled);

    // So does a 200 with another validator.
    let server = FakeServer::new(ServerConfig::new(40 * MIB));
    let reader = setup.cache.open(key("b"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
    server.update_config(|config| {
        config.ranges = false;
        config.validator = Some(Validator::ETag("\"v3\"".into()));
    });
    assert_eq!(read_once(&reader, 30 * MIB), ReadResult::Pending);
    server.pump();
    assert_eq!(
        read_once(&reader, 30 * MIB),
        ReadResult::Error(ReaderError::ResourceChanged)
    );

    // A new reader of the key gets the resource again.
    let server = FakeServer::new(ServerConfig::new(40 * MIB));
    let reader = setup.cache.open(key("b"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
}

#[test]
fn responses_from_several_origins_follow_verify_a_media_response() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    let events = events(&reader);
    read_expect(&server, &reader, 0, MIB, 0);
    assert!(!reader.cors_tainted());
    // A CORS-cross-origin response of the same origin taints the resource.
    server.update_config(|config| config.cors_tainted = true);
    read_expect(&server, &reader, 30 * MIB, MIB, 0);
    assert!(events.lock().contains(&ReaderEvent::Metadata {
        length: Some(100 * MIB),
        seekable: true,
        cors_tainted: true
    }));
    assert!(reader.cors_tainted());
    // A reader that opens later learns of the taint at once.
    assert!(setup.cache.open(key("a"), server.fetcher()).cors_tainted());
    // An untainted response from another origin makes it a resource of several origins.
    server.update_config(|config| {
        config.cors_tainted = false;
        config.origin = Some("https://other.example".into());
    });
    read_expect(&server, &reader, 50 * MIB, MIB, 0);
    // After that, a CORS-cross-origin response from any origin fails the load.
    server.update_config(|config| {
        config.cors_tainted = true;
        config.origin = Some("https://third.example".into());
    });
    assert_eq!(
        read_settled(&server, &reader, 70 * MIB, 1000),
        ReadResult::Error(ReaderError::Network)
    );

    // Responses without a URL match each other.
    let mut config = ServerConfig::new(100 * MIB);
    config.origin = None;
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("b"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
    server.update_config(|config| config.cors_tainted = true);
    read_expect(&server, &reader, 30 * MIB, MIB, 0);
    server.update_config(|config| config.origin = Some("https://example.com".into()));
    assert_eq!(
        read_settled(&server, &reader, 50 * MIB, 1000),
        ReadResult::Error(ReaderError::Network)
    );
}

#[test]
fn a_200_to_a_range_request_makes_the_resource_sequential() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    let events = events(&reader);
    read_expect(&server, &reader, 0, MIB, 0);
    server.pump();
    // The server stops supporting ranges, answering from another origin, CORS-cross-origin,
    // which a 200 may.
    server.update_config(|config| {
        config.ranges = false;
        config.origin = Some("https://other.example".into());
        config.cors_tainted = true;
    });
    read_expect(&server, &reader, 30 * MIB, MIB, 0);
    assert!(!reader.seekable());
    assert!(events.lock().contains(&ReaderEvent::Metadata {
        length: Some(100 * MIB),
        seekable: false,
        cors_tainted: true
    }));
    assert_eq!(server.requests().len(), 2);
    assert_eq!(server.state(0), FakeState::Cancelled);
    // What was cached before is still there, and the bytes before the read were discarded.
    let blocks = setup.block_indices(&key("a"));
    assert!(blocks.contains(&0));
    assert!(!blocks.contains(&(29 * MIB / BLOCK_SIZE)), "{blocks:?}");
    read_expect(&server, &reader, 0, MIB, 0);
}

#[test]
fn a_206_from_elsewhere_than_requested_does_not_restart_the_fetch_again_and_again() {
    let setup = setup(settings());
    // From 2 MiB before the requested start: the fetch reads on up to it.
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
    server.update_config(|config| config.answer_shift = 2 * MIB as i64);
    assert!(matches!(
        read_settled(&server, &reader, 30 * MIB, 10_000),
        ReadResult::Data(_)
    ));
    assert_eq!(server.requests().len(), 2);

    // From after it: the data cannot be had, which fails the load after one retry.
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let reader = setup.cache.open(key("b"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
    server.update_config(|config| config.answer_shift = -(BLOCK_SIZE as i64));
    assert_eq!(
        read_settled(&server, &reader, 30 * MIB + 100, 10_000),
        ReadResult::Error(ReaderError::Network)
    );
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn a_206_from_after_the_requested_position_is_forgotten_once_the_data_came() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
    for _ in 0..2 {
        // Once answered from after the requested position, then as requested.
        server.update_config(|config| config.answer_shift = -(BLOCK_SIZE as i64));
        assert_eq!(read_once(&reader, 30 * MIB + 100), ReadResult::Pending);
        server.update_config(|config| config.answer_shift = 0);
        assert!(matches!(
            read_settled(&server, &reader, 30 * MIB + 100, 10_000),
            ReadResult::Data(_)
        ));
        // The reader goes back, the fetch there is cancelled, and its data evicted.
        read_expect(&server, &reader, 0, 64 * KIB, 0);
        server.pump();
        setup.advance(31);
        setup.cache.trim(0);
        assert!(
            !setup
                .block_indices(&key("a"))
                .contains(&(30 * MIB / BLOCK_SIZE))
        );
    }
}

#[test]
fn early_end_is_recovered_once_per_fetch_that_made_progress() {
    let setup = setup(settings());
    let mut config = ServerConfig::new(20 * MIB);
    config.fail_after = Some(3 * MIB + 1234);
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("a"), server.fetcher());
    let events = events(&reader);
    // The restart also ends early after making progress, and is restarted again.
    server.update_config(|config| config.close_after = Some(MIB));
    read_expect(&server, &reader, 0, 5 * MIB, 0);
    let requests = server.requests();
    assert_eq!(requests[1].start, 3 * MIB + 1234);
    assert_eq!(requests[2].start, 4 * MIB + 1234);

    // A restart that delivers nothing is a network error.
    server.update_config(|config| config.close_after = Some(0));
    loop {
        match read_once(&reader, 5 * MIB + 1234) {
            ReadResult::Pending => assert!(server.step()),
            ReadResult::Error(error) => {
                assert_eq!(error, ReaderError::Network);
                break;
            },
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(
        events.lock().last(),
        Some(&ReaderEvent::Error(ReaderError::Network))
    );
}

#[test]
fn network_error_on_the_first_fetch() {
    let setup = setup(settings());
    let mut config = ServerConfig::new(20 * MIB);
    config.fail_after = Some(0);
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("a"), server.fetcher());
    assert_eq!(read_once(&reader, 0), ReadResult::Pending);
    server.pump();
    assert_eq!(
        read_once(&reader, 0),
        ReadResult::Error(ReaderError::Network)
    );
}

#[test]
fn range_not_satisfiable_ends_the_resource() {
    let setup = setup(settings());
    let length = 10 * MIB + 77;
    let mut config = ServerConfig::new(length);
    // Content-Length is too large; the body ends early.
    config.announced_length = Some(length + MIB);
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, length, 0);
    loop {
        match read_once(&reader, length) {
            ReadResult::EndOfStream => break,
            ReadResult::Pending => assert!(server.step()),
            other => panic!("{other:?}"),
        }
    }
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].start, length);
    assert_eq!(reader.length(), Some(length));
}

#[test]
fn eviction_keeps_the_read_ahead_and_drops_played_data_first() {
    let mut settings = settings();
    // 32 slots, so a single reader protects 1 MiB ahead, and two 512 KiB each.
    settings.file_budget = 2 * MIB;
    settings.memory_resource_limit = 0;
    let setup = setup(settings);
    let (a, b) = (key("a"), key("b"));
    let server_a = FakeServer::new(ServerConfig::new(12 * MIB));
    let reader_a = setup.cache.open(a.clone(), server_a.fetcher());
    read_expect(&server_a, &reader_a, 0, MIB, 0);
    server_a.pump();
    assert_eq!(setup.block_indices(&a), (0..32).collect::<Vec<_>>());
    // A slow reader: 30 s of read-ahead is 300 kB, blocks 17 to 20, and its blocks rank far
    // in the future.
    reader_a.set_activity(ReaderActivity::Playing {
        bytes_per_second: Some(10_000.),
    });

    let server_b = FakeServer::new(ServerConfig::new(12 * MIB));
    let reader_b = setup.cache.open(b.clone(), server_b.fetcher());
    read_expect(&server_b, &reader_b, 0, 512 * KIB, 0);
    server_b.pump();
    assert_eq!(setup.block_indices(&b).len(), 16);
    let blocks_a = setup.block_indices(&a);
    // Played data goes first, and before data as far ahead: 0 to 10 are further behind than
    // 21 to 27 are ahead.
    for index in 0..=10 {
        assert!(
            !blocks_a.contains(&index),
            "played block {index}: {blocks_a:?}"
        );
    }
    for index in 16..=27 {
        assert!(
            blocks_a.contains(&index),
            "block {index} ahead: {blocks_a:?}"
        );
    }

    // B plays on and needs more room than the unprotected blocks of A give. Its own played
    // data goes, but the read-ahead of A stays although it ranks later.
    read_expect(&server_b, &reader_b, 512 * KIB, 1536 * KIB, 0);
    server_b.pump();
    let blocks_a = setup.block_indices(&a);
    for index in 16..=20 {
        assert!(
            blocks_a.contains(&index),
            "read-ahead block {index}: {blocks_a:?}"
        );
    }
    let blocks_b = setup.block_indices(&b);
    assert!(!blocks_b.contains(&0), "{blocks_b:?}");
    for index in 32..40 {
        assert!(
            blocks_b.contains(&index),
            "read-ahead block {index} of b: {blocks_b:?}"
        );
    }
}

#[test]
fn only_readers_that_load_ahead_share_the_read_ahead() {
    let mut settings = settings();
    settings.file_budget = 4 * MIB;
    settings.memory_resource_limit = 0;
    let setup = setup(settings);
    // A reader of a complete resource and one that only needs the metadata.
    let server = FakeServer::new(ServerConfig::new(512 * KIB));
    let complete = setup.cache.open(key("complete"), server.fetcher());
    read_expect(&server, &complete, 0, 512 * KIB, 0);
    server.pump();
    let server = FakeServer::new(ServerConfig::new(40 * MIB));
    let metadata = setup.cache.open(key("metadata"), server.fetcher());
    metadata.set_activity(ReaderActivity::Metadata);
    server.pump();
    // So the one playing reader may protect half the budget.
    let server = FakeServer::new(ServerConfig::new(40 * MIB));
    let _playing = setup.cache.open(key("playing"), server.fetcher());
    server.pump();
    let sent = server.sent(0);
    assert!(
        (2 * MIB..2 * MIB + BLOCK_SIZE + 16 * KIB).contains(&sent),
        "{sent}"
    );
}

#[test]
fn blocks_read_for_the_metadata_are_evicted_by_last_use() {
    let mut settings = settings();
    settings.file_budget = 2 * MIB;
    settings.memory_resource_limit = 0;
    let setup = setup(settings);
    let a = key("a");
    let server = FakeServer::new(ServerConfig::new(12 * MIB));
    let reader = setup.cache.open(a.clone(), server.fetcher());
    // The demuxer reads the index at the end, then playback starts.
    reader.set_activity(ReaderActivity::Metadata);
    read_expect(&server, &reader, 12 * MIB - 1000, 1000, 0);
    reader.set_activity(ReaderActivity::Playing {
        bytes_per_second: None,
    });
    setup.advance(3);
    read_expect(&server, &reader, 0, 512 * KIB, 0);
    server.pump();
    let (_server, _other) = fill_with_other_resource(&setup.cache, "b", 7);
    // The index was used just now, unlike what lies far ahead of the reader.
    let last = (12 * MIB - 1) / BLOCK_SIZE;
    assert!(setup.block_indices(&a).contains(&last));
}

#[test]
fn a_fetch_stops_at_cached_data_unless_it_is_near() {
    let setup = setup(settings());
    let mut config = ServerConfig::new(40 * MIB);
    config.decline_suspend = true;
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("a"), server.fetcher());
    server.step();
    // Probes at 3 MiB and 20 MiB of a slow reader load 320 KiB each.
    reader.set_activity(ReaderActivity::Playing {
        bytes_per_second: Some(10_000.),
    });
    read_expect(&server, &reader, 3 * MIB, 1000, 0);
    server.pump();
    assert_eq!(server.state(0), FakeState::Cancelled);
    assert_eq!(server.state(1), FakeState::Cancelled);
    read_expect(&server, &reader, 20 * MIB, 1000, 0);
    server.pump();
    assert_eq!(server.requests().len(), 3);
    // Playing there, once the probes are no longer recent, loads 8 MiB more.
    setup.advance(3);
    reader.set_activity(ReaderActivity::Playing {
        bytes_per_second: None,
    });
    server.pump();
    assert_eq!(server.requests().len(), 4);
    let cached_end = 20 * MIB + server.sent(2) + server.sent(3);

    // Playback from the start reads through the data at 3 MiB, since what follows it is
    // near, and stops at the data at 20 MiB instead of suspending there.
    server.update_config(|config| config.decline_suspend = false);
    read_expect(&server, &reader, 0, 19 * MIB, 0);
    server.pump();
    assert_eq!(server.requests().len(), 5);
    assert_eq!(server.state(4), FakeState::Cancelled);
    assert_eq!(server.sent(4), 20 * MIB);
    // Once the cached data runs low, the next fetch starts after it.
    read_expect(&server, &reader, 19 * MIB, 8 * MIB, 0);
    let requests = server.requests();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[5].start, cached_end);
}

#[test]
fn small_resources_stay_in_memory() {
    let setup = setup(settings());
    let small = FakeServer::new(ServerConfig::new(8 * MIB));
    let reader = setup.cache.open(key("small"), small.fetcher());
    read_expect(&small, &reader, 0, 8 * MIB, 0);
    {
        let state = setup.cache.inner.state.lock();
        assert_eq!(state.store.used(), 0);
        assert!(!state.store.file_created());
        let res = state.resource(&key("small")).unwrap();
        assert!(res.ram_backed);
        assert!(res.blocks.values().all(|block| block.slot.is_none()));
    }

    let large = FakeServer::new(ServerConfig::new(8 * MIB + 1));
    let reader = setup.cache.open(key("large"), large.fetcher());
    read_expect(&large, &reader, 0, 4 * MIB, 0);
    let state = setup.cache.inner.state.lock();
    assert!(state.store.file_created());
    assert!(state.store.used() >= (4 * MIB / BLOCK_SIZE) as usize);
    let res = state.resource(&key("large")).unwrap();
    assert!(!res.ram_backed);
    // Full blocks went to the file.
    assert!(res.blocks[&0].file_len == BLOCK_SIZE as usize && res.blocks[&0].ram.is_empty());
}

#[test]
fn small_resources_share_a_memory_budget() {
    let mut settings = settings();
    settings.memory_fallback = 10 * MIB;
    let setup = setup(settings);
    let server = FakeServer::new(ServerConfig::new(4 * MIB));
    let unused = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &unused, 0, 4 * MIB, 0);
    server.pump();
    drop(unused);
    setup.advance(1);
    let b = setup.cache.open(key("b"), server.fetcher());
    read_expect(&server, &b, 0, 4 * MIB, 0);
    server.pump();

    // A third resource makes room by dropping the one without readers.
    let c = setup.cache.open(key("c"), server.fetcher());
    read_expect(&server, &c, 0, MIB, 0);
    {
        let state = setup.cache.inner.state.lock();
        assert!(state.resource(&key("a")).is_none());
        assert!(state.resource(&key("c")).unwrap().ram_backed);
        assert_eq!(state.store.used(), 0);
    }

    // A fourth one goes to the file.
    let d = setup.cache.open(key("d"), server.fetcher());
    read_expect(&server, &d, 0, 4 * MIB, 0);
    let state = setup.cache.inner.state.lock();
    assert!(state.resource(&key("b")).unwrap().ram_backed);
    assert!(!state.resource(&key("d")).unwrap().ram_backed);
    assert_eq!(state.store.used(), (4 * MIB / BLOCK_SIZE) as usize);
}

#[test]
fn a_small_resource_that_cannot_fit_in_memory_drops_nothing() {
    let mut settings = settings();
    settings.memory_fallback = 10 * MIB;
    let setup = setup(settings);
    let server = FakeServer::new(ServerConfig::new(4 * MIB));
    let unused = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &unused, 0, 4 * MIB, 0);
    server.pump();
    drop(unused);
    let used = setup.cache.open(key("b"), server.fetcher());
    read_expect(&server, &used, 0, 4 * MIB, 0);
    // 8 MiB fit with neither of them, so the one without readers stays.
    let server = FakeServer::new(ServerConfig::new(8 * MIB));
    let large = setup.cache.open(key("c"), server.fetcher());
    read_expect(&server, &large, 0, MIB, 0);
    let state = setup.cache.inner.state.lock();
    assert!(state.resource(&key("a")).is_some());
    assert!(!state.resource(&key("c")).unwrap().ram_backed);
}

#[test]
fn memory_fallback_is_one_budget_for_all_resources() {
    let mut settings = settings();
    settings.memory_fallback = 4 * MIB;
    settings.directory = Some("/nonexistent/media-cache".into());
    let setup = setup(settings);
    let server = FakeServer::new(ServerConfig::new(2 * MIB));
    let small = setup.cache.open(key("small"), server.fetcher());
    read_expect(&server, &small, 0, 2 * MIB, 0);
    let server = FakeServer::new(ServerConfig::new(20 * MIB));
    let large = setup.cache.open(key("large"), server.fetcher());
    read_expect(&server, &large, 0, 20 * MIB, 0);
    {
        let state = setup.cache.inner.state.lock();
        assert!(state.store.ram_mode());
        // The 2 MiB in memory leave 2 MiB of the fallback to the large resource.
        assert_eq!(state.store.capacity(), (2 * MIB / BLOCK_SIZE) as usize);
        assert!(state.store.used() <= (2 * MIB / BLOCK_SIZE) as usize);
    }
    let server = FakeServer::new(ServerConfig::new(MIB));
    let other = setup.cache.open(key("other"), server.fetcher());
    read_expect(&server, &other, 0, MIB, 0);
    let state = setup.cache.inner.state.lock();
    assert!(!state.resource(&key("other")).unwrap().ram_backed);
}

#[test]
fn file_creation_failure_falls_back_to_memory() {
    let mut settings = settings();
    settings.memory_fallback = 2 * MIB;
    settings.directory = Some("/nonexistent/media-cache".into());
    let setup = setup(settings);
    let server = FakeServer::new(ServerConfig::new(20 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, 20 * MIB, 0);
    let state = setup.cache.inner.state.lock();
    assert!(state.store.ram_mode());
    assert_eq!(state.store.capacity(), (2 * MIB / BLOCK_SIZE) as usize);
    assert!(state.store.used() <= (2 * MIB / BLOCK_SIZE) as usize);
    let res = state.resource(&key("a")).unwrap();
    assert!(res.blocks.values().all(|block| block.file_len == 0));
}

#[test]
fn a_read_error_of_the_file_keeps_blocks_in_memory() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(20 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, 2 * MIB, 0);
    setup
        .cache
        .inner
        .faults
        .fail_reads
        .store(true, Ordering::SeqCst);
    // The blocks in the file are lost, fetched again and kept in memory.
    read_expect(&server, &reader, 0, 4 * MIB, 0);
    assert!(setup.cache.inner.state.lock().store.ram_mode());
}

#[test]
fn a_write_error_of_the_file_keeps_blocks_in_memory() {
    let setup = setup(settings());
    setup
        .cache
        .inner
        .faults
        .fail_writes
        .store(true, Ordering::SeqCst);
    let server = FakeServer::new(ServerConfig::new(20 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, 20 * MIB, 0);
    let state = setup.cache.inner.state.lock();
    assert!(state.store.ram_mode());
    let res = state.resource(&key("a")).unwrap();
    assert!(res.blocks.values().all(|block| block.file_len == 0));
}

type OtherResource = (Arc<FakeServer>, MediaReader);

/// Opens `name` in `cache`, with data of `seed`, and reads 1.5 MiB of it.
fn fill_with_other_resource(
    cache: &MediaCache,
    name: &str,
    seed: u64,
) -> (Arc<FakeServer>, MediaReader) {
    let mut config = ServerConfig::new(12 * MIB);
    config.seed = seed;
    let server = FakeServer::new(config);
    let reader = cache.open(key(name), server.fetcher());
    read_expect(&server, &reader, 0, 1536 * KIB, seed);
    (server, reader)
}

#[test]
fn a_slot_released_while_written_is_not_reused_before_the_write() {
    let mut settings = settings();
    settings.file_budget = 2 * MIB;
    settings.memory_resource_limit = 0;
    let setup = setup(settings);
    let server = FakeServer::new(ServerConfig::new(12 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, 100 * KIB, 0);
    let slot: Arc<Mutex<Option<MediaReader>>> = Arc::new(Mutex::new(Some(reader)));
    let other: Arc<Mutex<Option<OtherResource>>> = Arc::default();
    let (cache, own, filled) = (
        MediaCache {
            inner: setup.cache.inner.clone(),
        },
        slot.clone(),
        other.clone(),
    );
    // While the next block is written, the resource goes, and another one takes the room.
    *setup.cache.inner.faults.before_write.lock() = Some(Box::new(move || {
        drop(own.lock().take());
        cache.trim(0);
        *filled.lock() = Some(fill_with_other_resource(&cache, "b", 7));
    }));
    while other.lock().is_none() {
        assert!(server.step());
    }
    let (server, reader) = other.lock().take().unwrap();
    read_expect(&server, &reader, 0, 1536 * KIB, 7);
}

#[test]
fn a_slot_released_while_read_is_not_reused_before_the_read() {
    let mut settings = settings();
    settings.file_budget = 2 * MIB;
    settings.memory_resource_limit = 0;
    let setup = setup(settings);
    let server = FakeServer::new(ServerConfig::new(12 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
    server.update_config(|config| config.validator = Some(Validator::ETag("\"v2\"".into())));
    assert_eq!(read_once(&reader, 8 * MIB), ReadResult::Pending);
    let other: Arc<Mutex<Option<OtherResource>>> = Arc::default();
    let (cache, changed, filled) = (
        MediaCache {
            inner: setup.cache.inner.clone(),
        },
        server.clone(),
        other.clone(),
    );
    // While a block is read, the resource changes and another one takes the room.
    *setup.cache.inner.faults.before_read.lock() = Some(Box::new(move || {
        assert!(changed.step_fetch(1));
        *filled.lock() = Some(fill_with_other_resource(&cache, "b", 7));
    }));
    let mut buffer = vec![0; 4096];
    assert_eq!(reader.read_at(0, &mut buffer), ReadResult::Data(4096));
    assert!(other.lock().is_some());
    for (pos, byte) in buffer.iter().enumerate() {
        assert_eq!(*byte, pattern(0, pos as u64), "byte at {pos}");
    }
    assert_eq!(
        read_once(&reader, 0),
        ReadResult::Error(ReaderError::ResourceChanged)
    );
}

#[test]
fn reuse_window() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(4 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, 4 * MIB, 0);
    server.pump();
    drop(reader);

    setup.advance(299);
    let reader = setup.cache.open(key("a"), server.fetcher());
    let events = events(&reader);
    read_expect(&server, &reader, 0, 4 * MIB, 0);
    assert_eq!(server.requests().len(), 1);
    assert_eq!(events.lock().last(), Some(&ReaderEvent::Complete));
    drop(reader);

    setup.advance(301);
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].if_range, None);
}

#[test]
fn unused_resources_are_dropped_after_the_reuse_window() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(4 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, 4 * MIB, 0);
    server.pump();
    drop(reader);
    setup.advance(299);
    assert!(setup.cache.inner.state.lock().resource(&key("a")).is_some());
    setup.advance(2);
    assert!(setup.cache.inner.state.lock().resource(&key("a")).is_none());
}

#[test]
fn last_reader_cancels_the_fetches() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(20 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &reader, 0, MIB, 0);
    drop(reader);
    assert_eq!(server.state(0), FakeState::Cancelled);
}

#[test]
fn a_closed_reader_is_detached_while_held_and_fails_its_reads() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(20 * MIB));
    let reader = Arc::new(setup.cache.open(key("a"), server.fetcher()));
    read_expect(&server, &reader, 0, MIB, 0);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    reader.set_waker(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    assert_eq!(read_once(&reader, 19 * MIB), ReadResult::Pending);
    let held_by_player = reader.clone();
    reader.close();
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert!(
        server
            .requests()
            .iter()
            .enumerate()
            .all(|(index, _)| server.state(index) == FakeState::Cancelled)
    );
    assert_eq!(
        read_once(&held_by_player, 0),
        ReadResult::Error(ReaderError::Network)
    );
    reader.close();
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    drop(reader);
    drop(held_by_player);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_fetch_does_not_outlive_the_reader_that_started_it() {
    let setup = setup(settings());
    let first_document = FakeServer::new(ServerConfig::new(20 * MIB));
    let second_document = FakeServer::new(ServerConfig::new(20 * MIB));
    let first = setup.cache.open(key("a"), first_document.fetcher());
    read_expect(&first_document, &first, 0, MIB, 0);
    let second = setup.cache.open(key("a"), second_document.fetcher());
    read_expect(&first_document, &second, 0, 512 * KIB, 0);
    assert_eq!(second_document.requests().len(), 0);
    let missing = first_document.sent(0);
    drop(first);
    assert_eq!(first_document.state(0), FakeState::Cancelled);
    let requests = second_document.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].start, missing);
    read_expect(&second_document, &second, 512 * KIB, 4 * MIB, 0);
}

#[test]
fn a_fetch_that_no_remaining_reader_reaches_is_cancelled() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(100 * MIB));
    let player = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &player, 0, MIB, 0);
    server.pump();
    let prober = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &prober, 50 * MIB, MIB, 0);
    server.pump();
    assert_eq!(server.state(1), FakeState::Suspended);
    drop(prober);
    assert_eq!(server.state(1), FakeState::Cancelled);
    assert_eq!(server.state(0), FakeState::Suspended);
}

#[test]
fn trim_drops_unused_resources_first() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(4 * MIB));
    let unused = setup.cache.open(key("a"), server.fetcher());
    read_expect(&server, &unused, 0, 4 * MIB, 0);
    server.pump();
    drop(unused);
    let used = setup.cache.open(key("b"), server.fetcher());
    read_expect(&server, &used, 0, 4 * MIB, 0);
    server.pump();
    setup.cache.trim(5 * MIB);
    let state = setup.cache.inner.state.lock();
    assert!(state.resource(&key("a")).is_none());
    assert_eq!(state.resource(&key("b")).unwrap().cached_bytes, 4 * MIB);
}

#[test]
fn wakers_are_called_once_per_pending_read_and_without_locks() {
    let setup = setup(settings());
    let mut config = ServerConfig::new(20 * MIB);
    config.chunk = 1000;
    let server = FakeServer::new(config);
    let reader = Arc::new(setup.cache.open(key("a"), server.fetcher()));
    let inner = Arc::downgrade(&setup.cache.inner);
    let weak: Weak<MediaReader> = Arc::downgrade(&reader);
    let wakes = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(Mutex::new(Vec::new()));
    let (counter, results) = (wakes.clone(), reads.clone());
    reader.set_waker(Box::new(move || {
        assert!(!inner.upgrade().unwrap().state.is_locked());
        counter.fetch_add(1, Ordering::SeqCst);
        // Reading from the waker must not deadlock.
        let reader = weak.upgrade().unwrap();
        results.lock().push(read_once(&reader, 0));
    }));
    let observed = Arc::new(AtomicUsize::new(0));
    let (counter, inner, weak) = (
        observed.clone(),
        Arc::downgrade(&setup.cache.inner),
        Arc::downgrade(&reader),
    );
    reader.set_observer(Box::new(move |_| {
        assert!(!inner.upgrade().unwrap().state.is_locked());
        counter.fetch_add(1, Ordering::SeqCst);
        let _ = read_once(&weak.upgrade().unwrap(), 0);
    }));

    assert_eq!(read_once(&reader, 0), ReadResult::Pending);
    assert_eq!(read_once(&reader, 0), ReadResult::Pending);
    server.step();
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
    // Many chunks arrive, one wake.
    for _ in 0..10 {
        server.step();
    }
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert_eq!(reads.lock()[0], ReadResult::Data(1000));
    assert!(observed.load(Ordering::SeqCst) > 0);

    // A new pending read at another position gets its own wake.
    assert_eq!(read_once(&reader, 15 * MIB), ReadResult::Pending);
    server.pump();
    assert_eq!(wakes.load(Ordering::SeqCst), 2);

    // An error wakes the reader too.
    let mut config = ServerConfig::new(20 * MIB);
    config.fail_after = Some(0);
    let server = FakeServer::new(config);
    let reader = setup.cache.open(key("b"), server.fetcher());
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    reader.set_waker(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    assert_eq!(read_once(&reader, 0), ReadResult::Pending);
    server.pump();
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_waker_set_after_the_data_arrived_is_called_at_once() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(20 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    assert_eq!(read_once(&reader, 0), ReadResult::Pending);
    server.step();
    server.step();
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    reader.set_waker(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    server.pump();
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
}

/// Counts how often what it is captured in is dropped while the cache's lock is held.
struct DropCheck {
    inner: Weak<crate::cache::Inner>,
    locked_drops: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

impl Drop for DropCheck {
    fn drop(&mut self) {
        if self
            .inner
            .upgrade()
            .is_some_and(|inner| inner.state.is_locked())
        {
            self.locked_drops.fetch_add(1, Ordering::SeqCst);
        }
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn callbacks_are_dropped_without_the_lock() {
    let setup = setup(settings());
    let locked_drops = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let check = || DropCheck {
        inner: Arc::downgrade(&setup.cache.inner),
        locked_drops: locked_drops.clone(),
        drops: drops.clone(),
    };
    let server = FakeServer::new(ServerConfig::new(20 * MIB));

    // Replaced wakers and observers, and those of a reader that is dropped.
    let reader = setup.cache.open(key("a"), server.fetcher());
    for _ in 0..2 {
        let guard = check();
        reader.set_waker(Box::new(move || {
            let _ = &guard;
        }));
        let guard = check();
        reader.set_observer(Box::new(move |_| {
            let _ = &guard;
        }));
    }
    drop(reader);
    assert_eq!(drops.load(Ordering::SeqCst), 4);

    // The queued events of a reader that another reader's observer drops.
    let second: Arc<Mutex<Option<MediaReader>>> = Arc::default();
    let first = setup.cache.open(key("b"), server.fetcher());
    let slot = second.clone();
    first.set_observer(Box::new(move |_| drop(slot.lock().take())));
    let reader = setup.cache.open(key("b"), server.fetcher());
    let guard = check();
    reader.set_observer(Box::new(move |_| {
        let _ = &guard;
    }));
    *second.lock() = Some(reader);
    server.step();
    assert!(second.lock().is_none());
    assert_eq!(drops.load(Ordering::SeqCst), 5);

    // Readers that drop themselves from their waker and their observer.
    for (name, in_waker) in [("c", true), ("d", false)] {
        let own: Arc<Mutex<Option<MediaReader>>> = Arc::default();
        let reader = setup.cache.open(key(name), server.fetcher());
        let (slot, guard) = (own.clone(), check());
        let drop_self = move || {
            let _ = &guard;
            drop(slot.lock().take());
        };
        if in_waker {
            reader.set_waker(Box::new(drop_self));
        } else {
            reader.set_observer(Box::new(move |_| drop_self()));
        }
        assert_eq!(read_once(&reader, 0), ReadResult::Pending);
        *own.lock() = Some(reader);
        server.pump();
        assert!(own.lock().is_none());
    }
    assert_eq!(drops.load(Ordering::SeqCst), 7);
    assert_eq!(locked_drops.load(Ordering::SeqCst), 0);
}

#[test]
fn a_waker_set_after_the_pending_data_was_read_is_not_called() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(20 * MIB));
    let reader = setup.cache.open(key("a"), server.fetcher());
    assert_eq!(read_once(&reader, 0), ReadResult::Pending);
    server.step();
    server.step();
    assert!(matches!(read_once(&reader, 0), ReadResult::Data(_)));
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    reader.set_waker(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
}

#[test]
fn readers_on_other_threads() {
    let setup = setup(settings());
    let server = FakeServer::new(ServerConfig::new(12 * MIB));
    let readers: Vec<_> = (0..3)
        .map(|_| Arc::new(setup.cache.open(key("a"), server.fetcher())))
        .collect();
    let threads: Vec<_> = readers
        .iter()
        .map(|reader| {
            let reader = reader.clone();
            std::thread::spawn(move || {
                let mut buffer = vec![0; 64 * 1024];
                let mut pos = 0;
                while pos < 12 * MIB {
                    match reader.read_at(pos, &mut buffer) {
                        ReadResult::Data(count) => {
                            for (offset, byte) in buffer[..count].iter().enumerate() {
                                assert_eq!(*byte, pattern(0, pos + offset as u64));
                            }
                            pos += count as u64;
                        },
                        ReadResult::Pending => std::thread::yield_now(),
                        other => panic!("{other:?}"),
                    }
                }
            })
        })
        .collect();
    while !threads.iter().all(|thread| thread.is_finished()) {
        if !server.step() {
            std::thread::yield_now();
        }
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(server.requests().len(), 1);
}
