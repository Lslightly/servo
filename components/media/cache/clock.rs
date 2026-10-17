/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use log::warn;
use parking_lot::{Condvar, Mutex, MutexGuard};

pub(crate) type TimerCallback = Box<dyn FnOnce() + Send>;

/// The time source of the cache and its timers, replaced by a fake one in tests.
pub(crate) trait Clock: Send + Sync {
    /// The time since a fixed point in the past.
    fn now(&self) -> Duration;
    /// Runs `callback` once `now()` reaches `at`. The callback runs on another thread, or in
    /// a fake clock when it is advanced, and never with a lock of the cache held.
    fn call_at(&self, at: Duration, callback: TimerCallback);
}

pub(crate) struct RealClock {
    shared: Arc<RealClockShared>,
}

struct RealClockShared {
    start: Instant,
    timers: Mutex<Timers>,
    condvar: Condvar,
}

#[derive(Default)]
struct Timers {
    pending: Vec<(Duration, TimerCallback)>,
    thread_started: bool,
}

impl RealClock {
    pub(crate) fn new() -> Self {
        Self {
            shared: Arc::new(RealClockShared {
                start: Instant::now(),
                timers: Mutex::default(),
                condvar: Condvar::new(),
            }),
        }
    }
}

impl Clock for RealClock {
    fn now(&self) -> Duration {
        self.shared.start.elapsed()
    }

    fn call_at(&self, at: Duration, callback: TimerCallback) {
        let mut timers = self.shared.timers.lock();
        timers.pending.push((at, callback));
        if !timers.thread_started {
            timers.thread_started = true;
            let shared = self.shared.clone();
            if let Err(error) = thread::Builder::new()
                .name("MediaCacheTimer".into())
                .spawn(move || shared.run())
            {
                warn!("Media cache: cannot start the timer thread: {error}");
            }
        }
        self.shared.condvar.notify_one();
    }
}

impl RealClockShared {
    fn run(&self) {
        let mut timers = self.timers.lock();
        loop {
            let now = self.start.elapsed();
            if let Some(index) = timers.pending.iter().position(|(at, _)| *at <= now) {
                let (_, callback) = timers.pending.swap_remove(index);
                MutexGuard::unlocked(&mut timers, callback);
                continue;
            }
            match timers.pending.iter().map(|(at, _)| *at).min() {
                Some(next) => {
                    self.condvar.wait_for(&mut timers, next - now);
                },
                None => self.condvar.wait(&mut timers),
            }
        }
    }
}

/// A clock that only moves when a test advances it, and then runs the timers that are due.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeClock {
    state: Mutex<FakeClockState>,
}

#[cfg(test)]
#[derive(Default)]
struct FakeClockState {
    now: Duration,
    pending: Vec<(Duration, TimerCallback)>,
}

#[cfg(test)]
impl FakeClock {
    pub(crate) fn advance(&self, by: Duration) {
        let due = {
            let mut state = self.state.lock();
            state.now += by;
            let now = state.now;
            let (mut due, pending): (Vec<_>, Vec<_>) =
                state.pending.drain(..).partition(|(at, _)| *at <= now);
            state.pending = pending;
            due.sort_by_key(|(at, _)| *at);
            due
        };
        for (_, callback) in due {
            callback();
        }
    }
}

#[cfg(test)]
impl Clock for FakeClock {
    fn now(&self) -> Duration {
        self.state.lock().now
    }

    fn call_at(&self, at: Duration, callback: TimerCallback) {
        self.state.lock().pending.push((at, callback));
    }
}
