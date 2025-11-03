// Copyright 2025 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

//! SQLite-based local cache storage backend.
//!
//! This module provides a SQLite-based implementation of local cache storage,
//! as an alternative to LMDB. SQLite may have better compatibility on some
//! filesystems and provides easier debugging capabilities.
//!
//! ## Blob Storage Strategy
//!
//! Following SQLite's recommendations (https://sqlite.org/intern-v-extern-blob.html),
//! we use a 100KB threshold:
//! - Blobs < 100KB: Stored directly in the database
//! - Blobs >= 100KB: Stored as separate files on disk
//!
//! ## Schema
//!
//! The database contains two tables:
//! - `entries`: Stores small blobs and metadata for all entries
//! - `leases`: Stores lease expiration timestamps for garbage collection

use bytes::Bytes;
use hashing::{AgedFingerprint, Digest, Fingerprint};
use log::warn;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default lease time for cache entries (2 hours).
pub const DEFAULT_LEASE_TIME: Duration = Duration::from_secs(2 * 60 * 60);

/// Threshold for storing blobs in the database vs. on disk (100KB).
/// Based on SQLite recommendations: https://sqlite.org/intern-v-extern-blob.html
const BLOB_SIZE_THRESHOLD: usize = 100 * 1024;

/// Schema version for the SQLite database.
const SCHEMA_VERSION: i32 = 1;

/// Default SQLite page size (4KB).
/// Common values: 4096 (4KB), 8192 (8KB), 16384 (16KB), 32768 (32KB).
pub const DEFAULT_PAGE_SIZE: u32 = 4096;

/// SQLite-based local cache storage.
///
/// This implementation uses a single SQLite database file with WAL mode
/// for concurrent access. Large blobs (>= 100KB) are stored as separate
/// files on disk.
#[derive(Debug)]
pub struct ShardedSqlite {
    /// Path to the database file.
    #[allow(dead_code)]
    db_path: PathBuf,
    /// Path to the directory for storing large blobs.
    blobs_dir: PathBuf,
    /// Database connection (wrapped in Arc<Mutex<>> for thread safety).
    conn: Arc<Mutex<Connection>>,
    /// Maximum size in bytes for the store.
    #[allow(dead_code)]
    max_size_bytes: usize,
    /// Lease time for entries.
    lease_time: Duration,
    /// SQLite page size in bytes.
    #[allow(dead_code)]
    page_size: u32,
}

impl ShardedSqlite {
    /// Create a new ShardedSqlite instance.
    ///
    /// # Arguments
    ///
    /// * `path` - Base directory for the store
    /// * `max_size_bytes` - Maximum size in bytes for the store
    /// * `lease_time` - Duration for which entries are leased
    /// * `page_size` - SQLite page size in bytes (must be a power of 2 between 512 and 65536)
    ///
    /// # Returns
    ///
    /// A Result containing the ShardedSqlite instance or an error string.
    pub fn new(
        path: PathBuf,
        max_size_bytes: usize,
        lease_time: Duration,
        page_size: u32,
    ) -> Result<Self, String> {
        // Create the base directory if it doesn't exist
        fs::create_dir_all(&path).map_err(|e| format!("Failed to create directory: {}", e))?;

        let db_path = path.join("cache.db");
        let blobs_dir = path.join("blobs");

        // Create blobs directory
        fs::create_dir_all(&blobs_dir)
            .map_err(|e| format!("Failed to create blobs directory: {}", e))?;

        // Open database connection
        let mut conn = Connection::open(&db_path)
            .map_err(|e| format!("Failed to open database: {}", e))?;

        // Check if database exists and has a different page size
        let existing_page_size: Option<u32> = conn
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .ok();

        if let Some(existing) = existing_page_size {
            if existing != page_size && existing != 0 {
                // Database exists with different page size - need to recreate
                warn!(
                    "Database page size mismatch (existing: {}, requested: {}). Recreating database.",
                    existing, page_size
                );
                drop(conn);

                // Remove old database files
                let _ = fs::remove_file(&db_path);
                // SQLite WAL files use -wal and -shm suffixes
                let mut wal_path = db_path.clone();
                wal_path.set_extension("db-wal");
                let _ = fs::remove_file(&wal_path);
                let mut shm_path = db_path.clone();
                shm_path.set_extension("db-shm");
                let _ = fs::remove_file(&shm_path);

                // Remove blobs directory
                let _ = fs::remove_dir_all(&blobs_dir);
                fs::create_dir_all(&blobs_dir)
                    .map_err(|e| format!("Failed to recreate blobs directory: {}", e))?;

                // Reopen database
                conn = Connection::open(&db_path)
                    .map_err(|e| format!("Failed to reopen database: {}", e))?;
            }
        }

        // Set page size (must be done before any tables are created)
        conn.pragma_update(None, "page_size", page_size)
            .map_err(|e| format!("Failed to set page size: {}", e))?;

        // Enable WAL mode for better concurrency
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| format!("Failed to enable WAL mode: {}", e))?;

        // Set synchronous mode to NORMAL for better performance
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| format!("Failed to set synchronous mode: {}", e))?;

        // Initialize schema
        Self::initialize_schema(&conn)?;

        Ok(Self {
            db_path,
            blobs_dir,
            conn: Arc::new(Mutex::new(conn)),
            max_size_bytes,
            lease_time,
            page_size,
        })
    }

    /// Initialize the database schema.
    fn initialize_schema(conn: &Connection) -> Result<(), String> {
        // Check if schema exists
        let version: Option<i32> = conn
            .query_row(
                "SELECT version FROM schema_version LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or(None);

        if let Some(v) = version {
            if v != SCHEMA_VERSION {
                return Err(format!(
                    "Incompatible schema version: expected {}, found {}",
                    SCHEMA_VERSION, v
                ));
            }
            return Ok(());
        }

        // Create schema
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS entries (
                fingerprint BLOB PRIMARY KEY NOT NULL,
                size_bytes INTEGER NOT NULL,
                data BLOB,
                last_accessed INTEGER NOT NULL
            ) WITHOUT ROWID;

            CREATE INDEX IF NOT EXISTS idx_last_accessed ON entries(last_accessed);

            CREATE TABLE IF NOT EXISTS leases (
                fingerprint BLOB PRIMARY KEY NOT NULL,
                lease_until INTEGER NOT NULL
            ) WITHOUT ROWID;
            "#,
        )
        .map_err(|e| format!("Failed to create schema: {}", e))?;

        conn.execute("INSERT INTO schema_version (version) VALUES (?1)", params![SCHEMA_VERSION])
            .map_err(|e| format!("Failed to insert schema version: {}", e))?;

        Ok(())
    }

    /// Get the path to a blob file for a given fingerprint.
    pub fn blob_path(&self, fingerprint: &Fingerprint) -> PathBuf {
        self.blobs_dir.join(fingerprint.to_hex())
    }

    /// Get the current Unix timestamp in seconds.
    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Store bytes in the database or as a file, depending on size.
    pub async fn store_bytes(
        &self,
        fingerprint: Fingerprint,
        bytes: Bytes,
        initial_lease: bool,
    ) -> Result<(), String> {
        let size = bytes.len();
        let now = Self::current_timestamp();
        let fingerprint_bytes = fingerprint.as_ref().to_vec();
        let fingerprint_hex = fingerprint.to_hex();

        let conn = self.conn.clone();
        let blobs_dir = self.blobs_dir.clone();
        let lease_time = self.lease_time;

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();

            if size < BLOB_SIZE_THRESHOLD {
                // Store in database
                conn.execute(
                    "INSERT OR REPLACE INTO entries (fingerprint, size_bytes, data, last_accessed) VALUES (?1, ?2, ?3, ?4)",
                    params![&fingerprint_bytes, size as i64, &bytes[..], now as i64],
                )
                .map_err(|e| format!("Failed to store entry: {}", e))?;
            } else {
                // Store as file
                let blob_path = blobs_dir.join(&fingerprint_hex);
                fs::write(&blob_path, &bytes)
                    .map_err(|e| format!("Failed to write blob file: {}", e))?;

                conn.execute(
                    "INSERT OR REPLACE INTO entries (fingerprint, size_bytes, data, last_accessed) VALUES (?1, ?2, NULL, ?3)",
                    params![&fingerprint_bytes, size as i64, now as i64],
                )
                .map_err(|e| format!("Failed to store entry metadata: {}", e))?;
            }

            if initial_lease {
                let lease_until = now + lease_time.as_secs();
                conn.execute(
                    "INSERT OR REPLACE INTO leases (fingerprint, lease_until) VALUES (?1, ?2)",
                    params![&fingerprint_bytes, lease_until as i64],
                )
                .map_err(|e| format!("Failed to store lease: {}", e))?;
            }

            Ok(())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    /// Load bytes from the database or file.
    pub async fn load_bytes_with<
        T: Send + 'static,
        F: FnMut(&[u8]) -> Result<T, String> + Send + Sync + 'static,
    >(
        &self,
        fingerprint: Fingerprint,
        mut f: F,
    ) -> Result<Option<T>, String> {
        let fingerprint_bytes = fingerprint.as_ref().to_vec();
        let conn = self.conn.clone();
        let blobs_dir = self.blobs_dir.clone();
        let now = Self::current_timestamp();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();

            // Update last accessed time
            conn.execute(
                "UPDATE entries SET last_accessed = ?1 WHERE fingerprint = ?2",
                params![now as i64, &fingerprint_bytes],
            )
            .map_err(|e| format!("Failed to update last accessed: {}", e))?;

            // Try to load from database first
            let result: Option<(Option<Vec<u8>>, i64)> = conn
                .query_row(
                    "SELECT data, size_bytes FROM entries WHERE fingerprint = ?1",
                    params![&fingerprint_bytes],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|e| format!("Failed to query entry: {}", e))?;

            match result {
                Some((Some(data), _)) => {
                    // Data is in database
                    Ok(Some(f(&data)?))
                }
                Some((None, _size)) => {
                    // Data is in file
                    let blob_path = blobs_dir.join(Fingerprint::from_bytes_unsafe(&fingerprint_bytes).to_hex());
                    let data = fs::read(&blob_path)
                        .map_err(|e| format!("Failed to read blob file: {}", e))?;
                    Ok(Some(f(&data)?))
                }
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    /// Extend or create a lease for a fingerprint.
    pub async fn lease(&self, fingerprint: Fingerprint) -> Result<(), String> {
        let fingerprint_bytes = fingerprint.as_ref().to_vec();
        let conn = self.conn.clone();
        let lease_time = self.lease_time;
        let now = Self::current_timestamp();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let lease_until = now + lease_time.as_secs();

            conn.execute(
                "INSERT OR REPLACE INTO leases (fingerprint, lease_until) VALUES (?1, ?2)",
                params![&fingerprint_bytes, lease_until as i64],
            )
            .map_err(|e| format!("Failed to update lease: {}", e))?;

            Ok(())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    /// Get all fingerprints with their expiration status.
    pub async fn all_fingerprints(&self) -> Result<Vec<AgedFingerprint>, String> {
        let conn = self.conn.clone();
        let now = Self::current_timestamp();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT e.fingerprint, e.size_bytes, COALESCE(l.lease_until, 0) as lease_until
                    FROM entries e
                    LEFT JOIN leases l ON e.fingerprint = l.fingerprint
                    "#,
                )
                .map_err(|e| format!("Failed to prepare statement: {}", e))?;

            let results = stmt
                .query_map([], |row| {
                    let fingerprint_bytes: Vec<u8> = row.get(0)?;
                    let size_bytes: i64 = row.get(1)?;
                    let lease_until: i64 = row.get(2)?;
                    Ok((fingerprint_bytes, size_bytes, lease_until))
                })
                .map_err(|e| format!("Failed to query fingerprints: {}", e))?;

            let mut aged_fingerprints = Vec::new();
            for result in results {
                let (fingerprint_bytes, size_bytes, lease_until) = result
                    .map_err(|e| format!("Failed to read row: {}", e))?;

                let fingerprint = Fingerprint::from_bytes_unsafe(&fingerprint_bytes);
                let expired_seconds_ago = if lease_until > 0 && (lease_until as u64) < now {
                    now - (lease_until as u64)
                } else {
                    0
                };

                aged_fingerprints.push(AgedFingerprint {
                    fingerprint,
                    expired_seconds_ago,
                    size_bytes: size_bytes as usize,
                });
            }

            Ok(aged_fingerprints)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    /// Remove an entry by its fingerprint. Returns true if the entry existed.
    pub async fn remove(&self, fingerprint: Fingerprint) -> Result<bool, String> {
        let fingerprint_bytes = fingerprint.as_ref().to_vec();
        let fingerprint_hex = fingerprint.to_hex();
        let conn = self.conn.clone();
        let blobs_dir = self.blobs_dir.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();

            // Check if entry exists and if data is stored as a file
            let has_file: Option<bool> = conn
                .query_row(
                    "SELECT data IS NULL FROM entries WHERE fingerprint = ?1",
                    params![&fingerprint_bytes],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| format!("Failed to check entry: {}", e))?;

            if has_file.is_none() {
                return Ok(false);
            }

            if let Some(true) = has_file {
                // Remove the blob file
                let blob_path = blobs_dir.join(fingerprint_hex);
                if let Err(e) = fs::remove_file(&blob_path) {
                    warn!("Failed to remove blob file {:?}: {}", blob_path, e);
                }
            }

            // Remove from database
            conn.execute(
                "DELETE FROM entries WHERE fingerprint = ?1",
                params![&fingerprint_bytes],
            )
            .map_err(|e| format!("Failed to delete entry: {}", e))?;

            conn.execute(
                "DELETE FROM leases WHERE fingerprint = ?1",
                params![&fingerprint_bytes],
            )
            .map_err(|e| format!("Failed to delete lease: {}", e))?;

            Ok(true)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    /// Check if an entry exists.
    pub async fn exists(&self, fingerprint: Fingerprint) -> Result<bool, String> {
        let fingerprint_bytes = fingerprint.as_ref().to_vec();
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM entries WHERE fingerprint = ?1",
                    params![&fingerprint_bytes],
                    |row| row.get(0),
                )
                .map_err(|e| format!("Failed to check existence: {}", e))?;

            Ok(count > 0)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    /// Check which fingerprints exist in a batch.
    pub async fn exists_batch(
        &self,
        fingerprints: Vec<Fingerprint>,
    ) -> Result<std::collections::HashSet<Fingerprint>, String> {
        if fingerprints.is_empty() {
            return Ok(std::collections::HashSet::new());
        }

        let fingerprint_bytes: Vec<Vec<u8>> = fingerprints
            .iter()
            .map(|f| f.as_ref().to_vec())
            .collect();
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut existing = std::collections::HashSet::new();

            for fp_bytes in &fingerprint_bytes {
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM entries WHERE fingerprint = ?1",
                        params![fp_bytes],
                        |row| row.get(0),
                    )
                    .map_err(|e| format!("Failed to check existence: {}", e))?;

                if count > 0 {
                    existing.insert(Fingerprint::from_bytes_unsafe(fp_bytes));
                }
            }

            Ok(existing)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    /// Store multiple entries in a batch.
    pub async fn store_bytes_batch(
        &self,
        items: Vec<(Fingerprint, Bytes)>,
        initial_lease: bool,
    ) -> Result<(), String> {
        for (fingerprint, bytes) in items {
            self.store_bytes(fingerprint, bytes, initial_lease).await?;
        }
        Ok(())
    }

    /// Store a file by reading it and storing its bytes.
    pub async fn store_file(
        &self,
        fingerprint: Fingerprint,
        path: PathBuf,
        initial_lease: bool,
    ) -> Result<(), String> {
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| format!("Failed to read file {:?}: {}", path, e))?;
        self.store_bytes(fingerprint, Bytes::from(bytes), initial_lease)
            .await
    }
}

#[cfg(test)]
mod tests;

