/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// The OHOS RDB backend keeps its own copy of the SQLite schemas, which only
// compiles for OHOS. These tests build both copies in SQLite on the host and
// compare the resulting schemas, so an upstream schema change that the RDB copy
// does not mirror fails here instead of silently diverging on devices.
// The webstorage `data` tables differ on purpose (the RDB one enforces unique,
// non-null keys) and are not compared.
#![cfg(feature = "sqlite-backend")]

use rusqlite::Connection;

use crate::client_storage_shared::CLIENT_STORAGE_SCHEMA_SQL;

/// The raw string literals of `source` that start with a `CREATE` statement,
/// in source order.
fn create_statements(source: &str) -> Vec<&str> {
    source
        .split("r#\"")
        .skip(1)
        .filter_map(|rest| rest.split_once("\"#").map(|(literal, _)| literal))
        .filter(|literal| {
            literal
                .trim_start()
                .get(..6)
                .is_some_and(|start| start.eq_ignore_ascii_case("create"))
        })
        .collect()
}

fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
        .replace(" if not exists", "")
}

fn rows(conn: &Connection, sql: &str, columns: usize) -> Vec<String> {
    let mut statement = conn.prepare(sql).unwrap();
    statement
        .query_map([], |row| {
            Ok((0..columns)
                .map(|i| {
                    let value: rusqlite::types::Value = row.get(i)?;
                    Ok(format!("{value:?}").to_ascii_lowercase())
                })
                .collect::<rusqlite::Result<Vec<_>>>()?
                .join("|"))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

/// A comparable description of every table and index of `conn`'s main schema.
fn describe_schema(conn: &Connection) -> Vec<String> {
    let mut description = vec![];
    let tables = rows(
        conn,
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        1,
    );
    for table in tables {
        let table = table.trim_start_matches("text(\"").trim_end_matches("\")");
        for (pragma, columns) in [
            ("table_list", 6),
            ("table_xinfo", 7),
            ("foreign_key_list", 8),
        ] {
            for row in rows(conn, &format!("PRAGMA {pragma}('{table}')"), columns) {
                description.push(format!("{table} {pragma}: {row}"));
            }
        }
        let mut indexes = vec![];
        let index_list = rows(
            conn,
            &format!("SELECT name, \"unique\", origin, partial FROM pragma_index_list('{table}')"),
            4,
        );
        for index in index_list {
            let name = index
                .split('|')
                .next()
                .unwrap()
                .trim_start_matches("text(\"")
                .trim_end_matches("\")")
                .to_owned();
            let columns = rows(
                conn,
                &format!("SELECT name FROM pragma_index_info('{name}') ORDER BY seqno"),
                1,
            )
            .join(",");
            // Implicit indexes are named after the order of the constraints in
            // the `CREATE TABLE` text, so only explicit ones are compared by name
            // and definition.
            let flags = index.split_once('|').unwrap().1;
            let identity = if name.starts_with("sqlite_autoindex_") {
                String::new()
            } else {
                let sql = rows(
                    conn,
                    &format!(
                        "SELECT sql FROM sqlite_schema WHERE type = 'index' AND name = '{name}'"
                    ),
                    1,
                );
                format!("{name} {}", normalize_sql(&sql.concat()))
            };
            indexes.push(format!("{table} index: {flags} on ({columns}) {identity}"));
        }
        indexes.sort();
        description.extend(indexes);
    }
    description
}

fn assert_same_schema(rdb: &[&str], sqlite: &[&str]) {
    let rdb = schema_of(rdb);
    let sqlite = schema_of(sqlite);
    let only_rdb: Vec<_> = rdb.iter().filter(|line| !sqlite.contains(line)).collect();
    let only_sqlite: Vec<_> = sqlite.iter().filter(|line| !rdb.contains(line)).collect();
    assert!(
        only_rdb.is_empty() && only_sqlite.is_empty(),
        "RDB schema differs from the SQLite schema.\nOnly in RDB: {only_rdb:#?}\nOnly in SQLite: {only_sqlite:#?}"
    );
}

fn schema_of(statements: &[&str]) -> Vec<String> {
    let conn = Connection::open_in_memory().unwrap();
    for statement in statements {
        conn.execute_batch(statement)
            .unwrap_or_else(|error| panic!("{error}: {statement}"));
    }
    describe_schema(&conn)
}

#[test]
fn indexeddb_rdb_schema_matches_sqlite_schema() {
    let sqlite = create_statements(include_str!("../indexeddb/engines/sqlite/create.rs"));
    let rdb = create_statements(include_str!("../indexeddb/engines/shared.rs"));
    assert_eq!(sqlite.len(), 6, "{sqlite:#?}");
    assert_eq!(rdb.len(), sqlite.len(), "{rdb:#?}");
    assert_same_schema(&rdb, &sqlite);
}

#[test]
fn client_storage_rdb_schema_matches_sqlite_schema() {
    let sqlite = create_statements(include_str!("../client_storage.rs"));
    assert!(sqlite.len() >= 7, "{sqlite:#?}");
    assert_same_schema(&CLIENT_STORAGE_SCHEMA_SQL, &sqlite);
}

#[test]
fn schema_comparison_sees_constraint_changes() {
    let global = ["CREATE TABLE t (id INTEGER PRIMARY KEY, store INTEGER, name TEXT UNIQUE);"];
    let per_store = [
        "CREATE TABLE t (id INTEGER PRIMARY KEY, store INTEGER, name TEXT, UNIQUE (store, name));",
    ];
    assert_ne!(schema_of(&global), schema_of(&per_store));
    let reformatted = [
        "CREATE TABLE IF NOT EXISTS t (\n id INTEGER PRIMARY KEY,\n store INTEGER,\n name TEXT unique\n);",
    ];
    assert_eq!(schema_of(&global), schema_of(&reformatted));
}
