/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The [`CacheStore`] contract, exercised against every backend.
//!
//! `store_suite!` instantiates the whole suite once per store, so a behaviour
//! that only the memory store or only the disk store gets right fails here.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use futures::StreamExt;
use http::{HeaderMap, HeaderValue, StatusCode, header};
use net::http_cache::CacheKey;
use net::http_cache::disk::DiskStore;
use net::http_cache::memory_store::MemoryStore;
use net::http_cache::policy::EntryPolicy;
use net::http_cache::store::{
    BodyRange, CACHE_FORMAT, CacheStore, EntryMeta, EntryWriter, StoreError,
};
use net_traits::blob_url_store::UrlWithBlobClaim;
use net_traits::request::{Referrer, Request, RequestBuilder};
use servo_base::id::TEST_PIPELINE_ID;
use servo_url::ServoUrl;
use tempfile::TempDir;

const BUDGET: u64 = 4 * 1024 * 1024;

fn request_for(url: &ServoUrl) -> Request {
    RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url.clone(), None),
        Referrer::NoReferrer,
    )
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .origin(url.origin())
    .build()
}

fn request_with_headers(url: &ServoUrl, headers: HeaderMap) -> Request {
    RequestBuilder::new(
        None,
        UrlWithBlobClaim::new(url.clone(), None),
        Referrer::NoReferrer,
    )
    .pipeline_id(Some(TEST_PIPELINE_ID))
    .origin(url.origin())
    .headers(headers)
    .build()
}

fn meta_for(url: &str, headers: &[(header::HeaderName, &str)]) -> EntryMeta {
    let url = ServoUrl::parse(url).unwrap();
    let request = request_for(&url);
    let mut header_map = HeaderMap::new();
    header_map.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=1000"),
    );
    for (name, value) in headers {
        header_map.insert(name.clone(), HeaderValue::from_str(value).unwrap());
    }
    EntryMeta {
        format: CACHE_FORMAT,
        stamp: rand::random(),
        key: CacheKey::from_url(url),
        policy: EntryPolicy::new(&request, StatusCode::OK, &header_map, SystemTime::now())
            .expect("storable"),
        headers: header_map.into(),
        status: StatusCode::OK.into(),
        content_encoding: None,
        body_len: 0,
    }
}

/// Wait until the writer can take another chunk, which is how `TeeBody` paces the
/// network onto the store.
async fn ready(writer: &mut EntryWriter) {
    futures::future::poll_fn(|cx| writer.poll_ready(cx)).await;
}

/// Store a body under `url` and return the entry it was committed as.
async fn put(store: &dyn CacheStore, url: &str, body: &[u8]) -> u64 {
    let mut writer = store.create(meta_for(url, &[])).await.expect("create");
    writer.push(Bytes::copy_from_slice(body));
    writer.commit().await.expect("commit")
}

async fn read_all(store: &dyn CacheStore, id: u64, meta: &EntryMeta) -> Vec<u8> {
    let mut stream = store
        .open(id, meta, None)
        .await
        .expect("open")
        .into_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.expect("read"));
    }
    body
}

async fn lookup_one(store: &dyn CacheStore, url: &str) -> Option<(u64, EntryMeta)> {
    let key = CacheKey::from_url(ServoUrl::parse(url).unwrap());
    store.lookup(&key).await.into_iter().next()
}

/// The tests below run against each of these; the disk store keeps its directory
/// alive for as long as the store.
struct StoreUnderTest {
    store: Arc<dyn CacheStore>,
    _dir: Option<TempDir>,
}

fn memory_store() -> StoreUnderTest {
    StoreUnderTest {
        store: Arc::new(MemoryStore::new(BUDGET as usize)),
        _dir: None,
    }
}

fn disk_store() -> StoreUnderTest {
    let dir = TempDir::new().expect("temporary directory");
    let store = DiskStore::open(dir.path().to_path_buf(), BUDGET).expect("open disk store");
    StoreUnderTest {
        store: Arc::new(store),
        _dir: Some(dir),
    }
}

macro_rules! store_suite {
    ($name:ident, $make:expr) => {
        mod $name {
            use super::*;

            #[tokio::test]
            async fn commit_then_read() {
                let under_test = $make;
                let store = under_test.store.as_ref();
                let id = put(store, "https://servo.org/a", b"hello").await;
                let (found_id, meta) = lookup_one(store, "https://servo.org/a")
                    .await
                    .expect("the committed entry should be found");
                assert_eq!(found_id, id);
                assert_eq!(meta.body_len, 5);
                assert_eq!(read_all(store, id, &meta).await, b"hello");
            }

            #[tokio::test]
            async fn an_uncommitted_entry_is_invisible() {
                let under_test = $make;
                let store = under_test.store.as_ref();
                {
                    let mut writer = store
                        .create(meta_for("https://servo.org/a", &[]))
                        .await
                        .expect("create");
                    writer.push(Bytes::from_static(b"partial"));
                    writer.abort();
                }
                assert!(
                    lookup_one(store, "https://servo.org/a").await.is_none(),
                    "an aborted writer must leave nothing behind"
                );
            }

            #[tokio::test]
            async fn a_body_larger_than_one_batch_round_trips() {
                let under_test = $make;
                let store = under_test.store.as_ref();
                let body: Vec<u8> = (0..700_000u32).map(|index| index as u8).collect();
                let mut writer = store
                    .create(meta_for("https://servo.org/big", &[]))
                    .await
                    .expect("create");
                for chunk in body.chunks(64 * 1024) {
                    ready(&mut writer).await;
                    writer.push(Bytes::copy_from_slice(chunk));
                }
                let id = writer.commit().await.expect("commit");
                let (_, meta) = lookup_one(store, "https://servo.org/big").await.unwrap();
                assert_eq!(meta.body_len as usize, body.len());
                assert_eq!(read_all(store, id, &meta).await, body);
            }

            #[tokio::test]
            async fn several_variants_live_under_one_key() {
                let under_test = $make;
                let store = under_test.store.as_ref();
                put(store, "https://servo.org/v", b"one").await;
                put(store, "https://servo.org/v", b"two").await;
                let key = CacheKey::from_url(ServoUrl::parse("https://servo.org/v").unwrap());
                assert_eq!(store.lookup(&key).await.len(), 2);
            }

            #[tokio::test]
            async fn ranges_read_a_slice_of_the_body() {
                let under_test = $make;
                let store = under_test.store.as_ref();
                let id = put(store, "https://servo.org/r", b"0123456789").await;
                let (_, meta) = lookup_one(store, "https://servo.org/r").await.unwrap();
                let mut stream = store
                    .open(id, &meta, Some(BodyRange { start: 2, end: 5 }))
                    .await
                    .expect("open a range")
                    .into_stream();
                let mut body = Vec::new();
                while let Some(chunk) = stream.next().await {
                    body.extend_from_slice(&chunk.expect("read"));
                }
                assert_eq!(body, b"2345");
            }

            #[tokio::test]
            async fn a_reused_id_is_not_touched_on_behalf_of_its_old_entry() {
                let under_test = $make;
                let store = under_test.store.as_ref();
                let url = "https://servo.org/reused";
                let old_id = put(store, url, b"old").await;
                let (_, old_meta) = lookup_one(store, url).await.unwrap();
                store.remove(old_id, old_meta.stamp).await;
                let new_id = put(store, url, b"new body").await;
                assert_eq!(new_id, old_id, "the id of a removed entry is reused");

                assert!(store.open(new_id, &old_meta, None).await.is_err());
                assert!(store.update_meta(new_id, old_meta.clone()).await.is_err());
                store.remove(new_id, old_meta.stamp).await;

                let (_, new_meta) = lookup_one(store, url).await.expect("still stored");
                assert_eq!(new_meta.body_len, 8);
                assert_eq!(read_all(store, new_id, &new_meta).await, b"new body");
            }

            #[tokio::test]
            async fn update_meta_keeps_the_body() {
                let under_test = $make;
                let store = under_test.store.as_ref();
                let id = put(store, "https://servo.org/m", b"body").await;
                let (_, mut meta) = lookup_one(store, "https://servo.org/m").await.unwrap();
                let mut headers = meta.headers.0.clone();
                headers.insert(header::ETAG, HeaderValue::from_static("\"fresh\""));
                meta.headers = headers.into();
                store.update_meta(id, meta).await.expect("update_meta");

                let (_, meta) = lookup_one(store, "https://servo.org/m").await.unwrap();
                assert_eq!(
                    meta.headers.get(header::ETAG).map(|value| value.as_bytes()),
                    Some(&b"\"fresh\""[..])
                );
                assert_eq!(read_all(store, id, &meta).await, b"body");
            }

            #[tokio::test]
            async fn remove_and_clear() {
                let under_test = $make;
                let store = under_test.store.as_ref();
                let id = put(store, "https://servo.org/x", b"x").await;
                put(store, "https://servo.org/y", b"y").await;

                let (_, meta) = lookup_one(store, "https://servo.org/x").await.unwrap();
                store.remove(id, meta.stamp).await;
                assert!(lookup_one(store, "https://servo.org/x").await.is_none());
                assert!(lookup_one(store, "https://servo.org/y").await.is_some());

                store.clear().await;
                assert!(lookup_one(store, "https://servo.org/y").await.is_none());
                assert!(store.descriptors().await.is_empty());
            }

            #[tokio::test]
            async fn remove_key_drops_every_variant() {
                let under_test = $make;
                let store = under_test.store.as_ref();
                put(store, "https://servo.org/k", b"one").await;
                put(store, "https://servo.org/k", b"two").await;
                let key = CacheKey::from_url(ServoUrl::parse("https://servo.org/k").unwrap());
                store.remove_key(&key).await;
                assert!(store.lookup(&key).await.is_empty());
            }

            #[tokio::test]
            async fn the_byte_budget_evicts_the_least_recently_used() {
                let under_test = $make;
                let store = under_test.store.as_ref();
                let chunk = vec![0u8; 512 * 1024];
                for index in 0..12 {
                    put(store, &format!("https://servo.org/e{index}"), &chunk).await;
                }
                let mut kept = 0;
                for index in 0..12 {
                    if lookup_one(store, &format!("https://servo.org/e{index}"))
                        .await
                        .is_some()
                    {
                        kept += 1;
                    }
                }
                assert!(
                    kept < 12,
                    "6 MiB of entries must not all fit in a 4 MiB budget"
                );
                assert!(kept > 0, "eviction must not empty the store");
                assert!(
                    lookup_one(store, "https://servo.org/e11").await.is_some(),
                    "the most recently written entry must survive"
                );
            }

            #[tokio::test]
            async fn concurrent_writers_of_one_key_get_separate_slots() {
                let under_test = $make;
                let store = under_test.store.clone();
                let first = store
                    .create(meta_for("https://servo.org/c", &[]))
                    .await
                    .expect("first create");
                let second = store
                    .create(meta_for("https://servo.org/c", &[]))
                    .await
                    .expect("second create");
                let first = tokio::spawn(async move { first.commit().await });
                let second = tokio::spawn(async move { second.commit().await });
                let first = first.await.unwrap().expect("first commit");
                let second = second.await.unwrap().expect("second commit");
                assert_ne!(first, second, "concurrent writers must not share a slot");
            }

            #[tokio::test]
            async fn descriptors_name_the_stored_urls() {
                let under_test = $make;
                let store = under_test.store.as_ref();
                put(store, "https://servo.org/d", b"d").await;
                let descriptors = store.descriptors().await;
                assert_eq!(descriptors.len(), 1);
            }
        }
    };
}

store_suite!(memory, memory_store());
store_suite!(disk, disk_store());

// The tests below are about the disk store's own failure modes: the file system
// is shared with the rest of the system, which is allowed to remove files and
// whole directories underneath a running cache.

fn open_disk_store(dir: &PathBuf) -> Arc<dyn CacheStore> {
    Arc::new(DiskStore::open(dir.clone(), BUDGET).expect("open disk store"))
}

#[tokio::test]
async fn disk_entries_survive_reopening() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = open_disk_store(&path);
        put(store.as_ref(), "https://servo.org/p", b"persisted").await;
        store.shutdown().await;
    }
    let store = open_disk_store(&path);
    let (id, meta) = lookup_one(store.as_ref(), "https://servo.org/p")
        .await
        .expect("the entry should still be there after a restart");
    assert_eq!(read_all(store.as_ref(), id, &meta).await, b"persisted");
}

#[tokio::test]
async fn disk_entries_are_found_again_after_the_index_is_lost() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = open_disk_store(&path);
        put(store.as_ref(), "https://servo.org/rebuild", b"body").await;
        store.shutdown().await;
    }
    // A crash before the index was written, or a corrupt index, both look like this.
    fs::write(path.join("index-data"), b"not an index").unwrap();

    let store = open_disk_store(&path);
    let (id, meta) = lookup_one(store.as_ref(), "https://servo.org/rebuild")
        .await
        .expect("a lost index must be rebuilt from the entry files");
    assert_eq!(read_all(store.as_ref(), id, &meta).await, b"body");
}

#[tokio::test]
async fn an_unclean_exit_still_finds_the_entries() {
    // No `shutdown()`, so the version stamp still says "in use" on the next open
    // and the index cannot be trusted -- the entries must come back by scanning.
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = open_disk_store(&path);
        put(store.as_ref(), "https://servo.org/unclean", b"body").await;
        // deliberately no shutdown/flush
    }
    let store = open_disk_store(&path);
    let (id, meta) = lookup_one(store.as_ref(), "https://servo.org/unclean")
        .await
        .expect("an unclean exit must not lose entries");
    assert_eq!(read_all(store.as_ref(), id, &meta).await, b"body");
}

#[tokio::test]
async fn entries_removed_while_closed_are_not_served_from_a_clean_index() {
    // The dirty flag says the index was up to date, but the system emptied the
    // directory while Servo was not running -- which is exactly what OpenHarmony
    // does to `cacheDir`. The directory mtime has to catch that.
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = open_disk_store(&path);
        put(store.as_ref(), "https://servo.org/a", b"a").await;
        put(store.as_ref(), "https://servo.org/b", b"b").await;
        store.flush().await;
    }
    for entry in fs::read_dir(path.join("entries")).unwrap().flatten() {
        fs::remove_file(entry.path()).unwrap();
    }

    let store = open_disk_store(&path);
    assert!(
        lookup_one(store.as_ref(), "https://servo.org/a")
            .await
            .is_none(),
        "an index that predates an external wipe must not claim the entries exist"
    );
    assert!(
        lookup_one(store.as_ref(), "https://servo.org/b")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn a_flush_makes_the_index_reusable() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = open_disk_store(&path);
        put(store.as_ref(), "https://servo.org/flushed", b"flushed").await;
        store.flush().await;
    }
    assert!(
        path.join("index-data").exists(),
        "flush must write the index out"
    );
    let store = open_disk_store(&path);
    let (id, meta) = lookup_one(store.as_ref(), "https://servo.org/flushed")
        .await
        .expect("a flushed index must still describe the entry");
    assert_eq!(read_all(store.as_ref(), id, &meta).await, b"flushed");
}

#[tokio::test]
async fn a_temporary_file_left_by_a_killed_writer_is_cleaned_up() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    let entries = path.join("entries");
    {
        let store = open_disk_store(&path);
        let mut writer = store
            .create(meta_for("https://servo.org/killed", &[]))
            .await
            .expect("create");
        writer.push(Bytes::from_static(b"never committed"));
        // Dropping the writer without committing is what a killed fetch does.
        drop(writer);
        store.shutdown().await;
    }
    // Leave a stray temporary file behind, as a killed *process* would.
    fs::write(entries.join("deadbeefdeadbe_0.tmp"), b"partial").unwrap();

    let store = open_disk_store(&path);
    assert!(
        lookup_one(store.as_ref(), "https://servo.org/killed")
            .await
            .is_none(),
        "an entry that was never committed must not be visible"
    );
    assert!(
        !entries.join("deadbeefdeadbe_0.tmp").exists(),
        "stray temporary files must be removed when the store is opened"
    );
}

#[tokio::test]
async fn an_entry_file_removed_behind_our_back_reads_as_a_miss() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    let store = open_disk_store(&path);
    put(store.as_ref(), "https://servo.org/gone", b"body").await;

    for entry in fs::read_dir(path.join("entries")).unwrap().flatten() {
        fs::remove_file(entry.path()).unwrap();
    }
    assert!(
        lookup_one(store.as_ref(), "https://servo.org/gone")
            .await
            .is_none(),
        "a removed entry file must be reported as a miss, not an error"
    );
}

#[tokio::test]
async fn the_whole_directory_can_be_removed_while_the_store_runs() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    let store = open_disk_store(&path);
    put(store.as_ref(), "https://servo.org/before", b"before").await;

    // This is what OpenHarmony does to `cacheDir` under storage pressure, and what
    // the user's "Clear cache" does even while the app is running.
    fs::remove_dir_all(&path).unwrap();

    assert!(
        lookup_one(store.as_ref(), "https://servo.org/before")
            .await
            .is_none()
    );
    let id = put(store.as_ref(), "https://servo.org/after", b"after").await;
    let (_, meta) = lookup_one(store.as_ref(), "https://servo.org/after")
        .await
        .expect("the store must recreate its directory and keep working");
    assert_eq!(read_all(store.as_ref(), id, &meta).await, b"after");
}

#[tokio::test]
async fn a_directory_written_by_another_format_is_discarded() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = open_disk_store(&path);
        put(store.as_ref(), "https://servo.org/old", b"old").await;
        store.shutdown().await;
    }
    // The version stamp of another format of this store: magic, format, clean.
    // There is no migration; the store's files go, and nothing else.
    let mut stamp = fs::read(path.join("index")).unwrap();
    stamp[8] ^= 0xff;
    fs::write(path.join("index"), &stamp).unwrap();
    fs::write(path.join("embedder-file"), b"not the cache's").unwrap();

    let store = open_disk_store(&path);
    assert!(
        lookup_one(store.as_ref(), "https://servo.org/old")
            .await
            .is_none(),
        "a format bump must discard the store's files rather than read them"
    );
    assert!(
        fs::read_dir(path.join("entries")).unwrap().next().is_none(),
        "the entry files of the old format must be gone"
    );
    assert!(
        path.join("embedder-file").exists(),
        "a file the store did not write must survive a format bump"
    );
}

#[tokio::test]
async fn a_directory_with_a_foreign_index_file_is_left_alone() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    fs::write(path.join("index"), b"an index of something else").unwrap();
    fs::create_dir_all(path.join("entries")).unwrap();
    fs::write(path.join("entries").join("data"), b"theirs").unwrap();

    assert!(
        DiskStore::open(path.clone(), BUDGET).is_err(),
        "a directory with someone else's `index` file is not the cache's"
    );
    assert_eq!(
        fs::read(path.join("index")).unwrap(),
        b"an index of something else"
    );
    assert_eq!(
        fs::read(path.join("entries").join("data")).unwrap(),
        b"theirs"
    );
}

#[tokio::test]
async fn a_second_store_cannot_open_a_directory_in_use() {
    // Two stores in one directory write the same temporary files and overwrite
    // each other's committed bodies.
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    let store = open_disk_store(&path);
    assert!(
        DiskStore::open(path.clone(), BUDGET).is_err(),
        "a directory another store has open must not be opened again"
    );
    drop(store);
    assert!(
        DiskStore::open(path.clone(), BUDGET).is_ok(),
        "closing the store must release its directory"
    );
}

#[tokio::test]
async fn a_writer_in_flight_when_the_index_is_flushed_leaves_nothing_behind() {
    // An application is backgrounded while a body is being written, which flushes
    // the index, and then killed. The next start trusts the index, so nothing
    // scans the directory for the dead writer's file.
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = open_disk_store(&path);
        put(store.as_ref(), "https://servo.org/a", b"a").await;
        let mut writer = store
            .create(meta_for("https://servo.org/in-flight", &[]))
            .await
            .expect("create");
        writer.push(Bytes::from(vec![7; 300 * 1024]));
        ready(&mut writer).await;
        store.flush().await;
        // Killed: the writer never commits or aborts.
        std::mem::forget(writer);
    }

    let store = open_disk_store(&path);
    let leftovers: u64 = fs::read_dir(&path)
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .flat_map(|dir| fs::read_dir(dir.path()).unwrap().flatten())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .map(|entry| entry.metadata().unwrap().len())
        .sum();
    assert_eq!(leftovers, 0, "the dead writer's file must not stay forever");
    assert!(
        lookup_one(store.as_ref(), "https://servo.org/a")
            .await
            .is_some()
    );
}

#[tokio::test]
async fn entries_removed_while_the_store_runs_are_not_kept_in_its_index() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let store = open_disk_store(&path);
        for i in 0..8 {
            let mut writer = store
                .create(meta_for(&format!("https://servo.org/{i}"), &[]))
                .await
                .expect("create");
            writer.push(Bytes::from(vec![0; 100 * 1024]));
            writer.commit().await.expect("commit");
        }
        // The system empties the cache directory while the store runs.
        fs::remove_dir_all(&path).unwrap();
        put(store.as_ref(), "https://servo.org/after", b"after").await;
        assert!(
            store.disk_bytes() < 100 * 1024,
            "the removed entries must not count towards the budget"
        );
        store.flush().await;
    }

    let store = open_disk_store(&path);
    let on_disk: u64 = fs::read_dir(path.join("entries"))
        .unwrap()
        .flatten()
        .map(|entry| entry.metadata().unwrap().len())
        .sum();
    assert_eq!(
        store.disk_bytes(),
        on_disk,
        "the index written after the removal must only hold what is on disk"
    );
}

#[tokio::test]
async fn a_corrupt_body_is_reported_rather_than_served() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    let store = open_disk_store(&path);
    let id = put(store.as_ref(), "https://servo.org/corrupt", b"0123456789").await;
    let (_, meta) = lookup_one(store.as_ref(), "https://servo.org/corrupt")
        .await
        .unwrap();

    // Flip a byte in the body, leaving the metadata and lengths intact.
    let entry = fs::read_dir(path.join("entries"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let mut bytes = fs::read(&entry).unwrap();
    bytes[20] ^= 0xff;
    fs::write(&entry, &bytes).unwrap();

    let mut stream = store
        .open(id, &meta, None)
        .await
        .expect("open")
        .into_stream();
    let mut failed = false;
    while let Some(chunk) = stream.next().await {
        failed |= chunk.is_err();
    }
    assert!(
        failed,
        "a body that fails its checksum must surface as an error, not as silent corruption"
    );
    assert!(
        lookup_one(store.as_ref(), "https://servo.org/corrupt")
            .await
            .is_none(),
        "a body that could not be read must be dropped, or every later fetch of that \
         URL fails the same way"
    );
}

#[tokio::test]
async fn credentials_are_never_written_to_an_entry() {
    // The policy is serialized into the entry, so this is what stops a session
    // cookie from living on disk for as long as the cache does.
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    let store = open_disk_store(&path);

    let url = ServoUrl::parse("https://user:SECRETPASSWORD@servo.org/private").unwrap();
    let mut request_headers = HeaderMap::new();
    request_headers.insert(header::COOKIE, HeaderValue::from_static("sid=SECRETCOOKIE"));
    request_headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Basic SECRETAUTH"),
    );
    let request = request_with_headers(&url, request_headers);

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=1000"),
    );
    headers.insert(
        header::VARY,
        HeaderValue::from_static("cookie, authorization"),
    );
    let meta = EntryMeta {
        format: CACHE_FORMAT,
        stamp: rand::random(),
        key: CacheKey::from_url(url),
        policy: EntryPolicy::new(&request, StatusCode::OK, &headers, SystemTime::now())
            .expect("storable"),
        headers: headers.into(),
        status: StatusCode::OK.into(),
        content_encoding: None,
        body_len: 0,
    };
    let mut writer = store.create(meta).await.expect("create");
    writer.push(Bytes::from_static(b"body"));
    writer.commit().await.expect("commit");

    let entry = fs::read_dir(path.join("entries"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let bytes = fs::read(&entry).unwrap();
    for secret in [
        &b"SECRETCOOKIE"[..],
        &b"SECRETAUTH"[..],
        &b"SECRETPASSWORD"[..],
    ] {
        assert!(
            !bytes.windows(secret.len()).any(|window| window == secret),
            "a credential reached the entry file: {}",
            String::from_utf8_lossy(secret)
        );
    }
}

#[tokio::test]
async fn a_hash_collision_does_not_serve_the_wrong_url() {
    // Entry files are named by a 56-bit hash of the key, and the full key is
    // compared on open. Renaming one entry's file onto another's name is exactly
    // what a collision looks like.
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    let store = open_disk_store(&path);
    put(store.as_ref(), "https://servo.org/one", b"one").await;

    let key = CacheKey::from_url(ServoUrl::parse("https://servo.org/two").unwrap());
    let entry = fs::read_dir(path.join("entries"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let colliding = path.join("entries").join(format!("{:014x}_0", key.hash()));
    fs::rename(&entry, &colliding).unwrap();

    // Reopen so the index picks the renamed file up.
    drop(store);
    let store = open_disk_store(&path);
    assert!(
        store.lookup(&key).await.is_empty(),
        "an entry whose stored key differs must not answer for this key"
    );
}

#[tokio::test]
async fn an_entry_larger_than_the_per_entry_cap_is_declined() {
    let dir = TempDir::new().unwrap();
    let store = open_disk_store(&dir.path().to_path_buf());
    let mut writer = store
        .create(meta_for("https://servo.org/huge", &[]))
        .await
        .expect("create");
    // The cap is max(budget / 8, 5 MiB), so 6 MiB is over it.
    let chunk = vec![0u8; 1024 * 1024];
    for _ in 0..6 {
        writer.push(Bytes::from(chunk.clone()));
    }
    assert!(
        matches!(writer.commit().await, Err(StoreError::Rejected)),
        "an outsized entry must be declined rather than written"
    );
    assert!(
        lookup_one(store.as_ref(), "https://servo.org/huge")
            .await
            .is_none()
    );
}
