/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! What the fetches of a resource should do, and which blocks to evict.

use std::time::Duration;

use crate::cache::Settings;
use crate::resource::{Block, Control, Fetch, FetchId, READ_THROUGH, Resource};
use crate::store::BLOCK_SIZE;

/// At most this many fetches run or are suspended per resource.
pub(crate) const MAX_FETCHES: usize = 2;

#[derive(Debug, PartialEq)]
pub(crate) enum Action {
    Suspend(FetchId),
    Resume(FetchId),
    Cancel(FetchId),
    Start { start: u64, discard_before: u64 },
}

/// A reader that has less than its read-ahead target loaded.
struct Demand {
    /// The first position after the reader's that is not cached.
    missing: u64,
    resume_at: u64,
    /// Whether the reader waits for data at its position.
    urgent: bool,
    /// Whether this is the reader's last position. Only that one starts fetches; the other
    /// recent ones keep fetches running or resume them.
    current: bool,
}

/// Decides how the fetches of `res` follow its readers. `cap` is the largest read-ahead of a
/// reader of a resource in the file, and `unseekable_share` the protected read-ahead of a
/// reader of a resource without range support.
pub(crate) fn decide(
    res: &Resource,
    settings: &Settings,
    cap: u64,
    unseekable_share: u64,
    now: Duration,
) -> Vec<Action> {
    let end = res.end();
    if res.error.is_some() || res.readers.is_empty() || res.is_complete() {
        return res
            .fetches
            .iter()
            .filter(|fetch| fetch.pos < end || res.error.is_some() || res.readers.is_empty())
            .map(|fetch| Action::Cancel(fetch.id))
            .collect();
    }
    if res.meta.is_none() {
        if res.fetches.is_empty() {
            return vec![Action::Start {
                start: 0,
                discard_before: 0,
            }];
        }
        return Vec::new();
    }

    let seekable = res.seekable();
    let cap = if res.ram_backed { u64::MAX } else { cap };
    let demands: Vec<Demand> = res
        .readers
        .values()
        .flat_map(|reader| {
            let (ahead, resume) = reader.thresholds(settings.readahead, settings.resume, cap);
            reader
                .positions(now)
                .into_iter()
                .filter(|pos| *pos < end)
                .filter_map(move |pos| {
                    let target = pos.saturating_add(ahead).min(end);
                    let missing = res.cached_until(pos, target);
                    let urgent = reader.pending == Some(pos) && missing == pos;
                    let current = pos == reader.pos;
                    // An idle reader only needs what it waits for.
                    if reader.is_idle() && !(urgent && current) {
                        return None;
                    }
                    (missing < target).then_some(Demand {
                        missing,
                        resume_at: pos.saturating_add(resume),
                        urgent,
                        current,
                    })
                })
        })
        .collect();
    // A fetch at `pos` that was requested at `start` reaches the data of a demand soon enough;
    // a 206 may start before the requested position. Without range support, the one fetch
    // reaches everything ahead of it.
    let serves = |pos: u64, start: u64, demand: &Demand| {
        pos <= demand.missing &&
            (!seekable || demand.missing - pos.max(start).min(demand.missing) <= READ_THROUGH)
    };

    // A fetch without range support whose suspension was declined keeps loading up to here,
    // since a restart would fetch the resource from its start again.
    let keep_until = if res.ram_backed {
        u64::MAX
    } else {
        res.readers
            .values()
            .map(|reader| reader.pos.saturating_add(unseekable_share))
            .max()
            .unwrap_or(0)
    };

    let mut actions = Vec::new();
    let mut live: Vec<&Fetch> = Vec::new();
    for fetch in &res.fetches {
        if fetch.pos >= end {
            live.push(fetch);
            continue;
        }
        if seekable && fetch.responded && fetch.pos >= fetch.start && res.is_cached(fetch.pos) {
            let next = res.cached_until(fetch.pos, fetch.pos + READ_THROUGH + 1);
            let extended_by_other = res
                .fetches
                .iter()
                .any(|other| other.id != fetch.id && other.pos == next);
            if next >= end || next - fetch.pos > READ_THROUGH || extended_by_other {
                actions.push(Action::Cancel(fetch.id));
                continue;
            }
        }
        let wanted = demands
            .iter()
            .any(|demand| serves(fetch.pos, fetch.start, demand));
        let resume = demands.iter().any(|demand| {
            serves(fetch.pos, fetch.start, demand) &&
                (demand.urgent || demand.missing < demand.resume_at)
        });
        match fetch.control {
            Control::Running if !wanted && (!fetch.declined || fetch.pos >= keep_until) => {
                actions.push(Action::Suspend(fetch.id))
            },
            Control::Suspending | Control::Suspended if resume => {
                actions.push(Action::Resume(fetch.id))
            },
            _ => {},
        }
        live.push(fetch);
    }

    let mut unserved: Vec<&Demand> = demands
        .iter()
        .filter(|demand| {
            !live
                .iter()
                .any(|fetch| serves(fetch.pos, fetch.start, demand))
        })
        .filter(|demand| {
            demand.current &&
                (demand.urgent || demand.missing < demand.resume_at || res.restart_now)
        })
        .collect();
    unserved.sort_by_key(|demand| (!demand.urgent, demand.missing));
    let distance = |fetch: &Fetch| {
        res.readers
            .values()
            .map(|reader| reader.pos.abs_diff(fetch.pos))
            .min()
            .unwrap_or(u64::MAX)
    };
    let mut started: Vec<u64> = Vec::new();
    for demand in unserved {
        if started.iter().any(|&start| serves(start, start, demand)) {
            continue;
        }
        let start = res.fetch_start(demand.missing);
        if !seekable {
            // The data is behind the one fetch, which cannot seek: fetch from the start of
            // the resource again, without keeping what lies before the data.
            actions.extend(live.drain(..).map(|fetch| Action::Cancel(fetch.id)));
            actions.push(Action::Start {
                start,
                discard_before: demand.missing / BLOCK_SIZE * BLOCK_SIZE,
            });
            break;
        }
        if live.len() + started.len() >= MAX_FETCHES {
            let Some((index, victim)) = live
                .iter()
                .enumerate()
                .max_by_key(|(_, fetch)| distance(fetch))
            else {
                continue;
            };
            let victim_serves = demands
                .iter()
                .any(|other| serves(victim.pos, victim.start, other));
            let victim_serves_urgent = demands
                .iter()
                .any(|other| other.urgent && serves(victim.pos, victim.start, other));
            if victim_serves && !(demand.urgent && !victim_serves_urgent) {
                continue;
            }
            actions.push(Action::Cancel(victim.id));
            live.remove(index);
        }
        actions.push(Action::Start {
            start,
            discard_before: 0,
        });
        started.push(start);
    }
    actions
}

/// A reader as eviction sees it.
pub(crate) struct ReaderView {
    pub pos: u64,
    pub rate: f64,
    /// The end of the read-ahead that must not be evicted.
    pub protect_end: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Tier {
    Evictable,
    /// In the read-ahead of a reader: evicted only if nothing else is left.
    Protected,
    /// At the position of a reader.
    Never,
}

/// How a block ranks for eviction: its tier, and when it is predicted to be used next, in
/// seconds, as Gecko's `PredictNextUse`. Blocks used later go first. Blocks read for the
/// metadata, and those of resources without readers, rank by their last use.
pub(crate) fn rank(views: &[ReaderView], index: u64, block: &Block, now: Duration) -> (Tier, f64) {
    if views.is_empty() {
        return (
            Tier::Evictable,
            now.saturating_sub(block.last_use).as_secs_f64(),
        );
    }
    let start = index * BLOCK_SIZE;
    let end = start + BLOCK_SIZE;
    let mut tier = Tier::Evictable;
    let mut next_use = f64::MAX;
    for view in views {
        if index == view.pos / BLOCK_SIZE {
            return (Tier::Never, 0.);
        }
        if start > view.pos && start < view.protect_end {
            tier = Tier::Protected;
        }
        let seconds = if start >= view.pos {
            (start - view.pos) as f64 / view.rate
        } else {
            view.pos.saturating_sub(end) as f64 / view.rate * 3.
        };
        next_use = next_use.min(seconds);
    }
    if block.metadata && tier == Tier::Evictable {
        next_use = now.saturating_sub(block.last_use).as_secs_f64();
    }
    (tier, next_use)
}
