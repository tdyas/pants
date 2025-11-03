// Copyright 2025 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

//! Micro-benchmarks comparing SQLite and LMDB backend performance.
//!
//! These benchmarks test the UnderlyingByteStore trait methods directly to isolate
//! backend performance from higher-level Store operations.

use bytes::Bytes;
use criterion::{criterion_group, BenchmarkId, Criterion, Throughput};
use hashing::Fingerprint;
use sharded_lmdb::ShardedLmdb;
use sharded_sqlite::{ShardedSqlite, DEFAULT_PAGE_SIZE};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use task_executor::Executor;
use tempfile::TempDir;

const DEFAULT_LEASE_TIME: Duration = Duration::from_secs(2 * 60 * 60);

// Test data generators
fn generate_data(size: usize) -> Bytes {
    Bytes::from(vec![42u8; size])
}

fn generate_fingerprint(seed: u64) -> Fingerprint {
    let mut bytes = [0u8; 32];
    bytes[0..8].copy_from_slice(&seed.to_le_bytes());
    Fingerprint::from_bytes_unsafe(&bytes)
}

// Backend wrapper for unified testing
enum Backend {
    Lmdb(Arc<ShardedLmdb>),
    Sqlite(Arc<ShardedSqlite>),
}

impl Backend {
    fn new_lmdb(path: &std::path::Path, max_size: usize, shard_count: u8) -> Self {
        let executor = Executor::new_owned(8, 32, || ()).unwrap();
        let lmdb = ShardedLmdb::new(
            path.to_path_buf(),
            max_size,
            executor,
            DEFAULT_LEASE_TIME,
            shard_count,
        )
        .unwrap();
        Backend::Lmdb(Arc::new(lmdb))
    }

    fn new_sqlite(path: &std::path::Path, max_size: usize) -> Self {
        let sqlite = ShardedSqlite::new(
            path.to_path_buf(),
            max_size,
            DEFAULT_LEASE_TIME,
            DEFAULT_PAGE_SIZE,
        )
        .unwrap();
        Backend::Sqlite(Arc::new(sqlite))
    }

    async fn store_bytes(
        &self,
        fingerprint: Fingerprint,
        data: Bytes,
        initial_lease: bool,
    ) -> Result<(), String> {
        self.store_bytes_batch(vec![(fingerprint, data)], initial_lease).await
    }

    async fn load_bytes_with<T: Send + 'static, F: Fn(&[u8]) -> Result<T, String> + Send + Sync + 'static>(
        &self,
        fingerprint: Fingerprint,
        f: F,
    ) -> Result<Option<T>, String> {
        match self {
            Backend::Lmdb(lmdb) => lmdb.load_bytes_with(fingerprint, f).await,
            Backend::Sqlite(sqlite) => sqlite.load_bytes_with(fingerprint, f).await,
        }
    }

    async fn exists(&self, fingerprint: Fingerprint) -> Result<bool, String> {
        match self {
            Backend::Lmdb(lmdb) => lmdb.exists(fingerprint).await,
            Backend::Sqlite(sqlite) => sqlite.exists(fingerprint).await,
        }
    }

    async fn exists_batch(&self, fingerprints: Vec<Fingerprint>) -> Result<Vec<bool>, String> {
        let exists_set = match self {
            Backend::Lmdb(lmdb) => lmdb.exists_batch(fingerprints.clone()).await?,
            Backend::Sqlite(sqlite) => sqlite.exists_batch(fingerprints.clone()).await?,
        };
        Ok(fingerprints.iter().map(|fp| exists_set.contains(fp)).collect())
    }

    async fn lease(&self, fingerprint: Fingerprint) -> Result<(), String> {
        match self {
            Backend::Lmdb(lmdb) => lmdb.lease(fingerprint).await,
            Backend::Sqlite(sqlite) => sqlite.lease(fingerprint).await,
        }
    }

    async fn remove(&self, fingerprint: Fingerprint) -> Result<bool, String> {
        match self {
            Backend::Lmdb(lmdb) => lmdb.remove(fingerprint).await,
            Backend::Sqlite(sqlite) => sqlite.remove(fingerprint).await,
        }
    }

    async fn store_bytes_batch(
        &self,
        items: Vec<(Fingerprint, Bytes)>,
        initial_lease: bool,
    ) -> Result<(), String> {
        match self {
            Backend::Lmdb(lmdb) => lmdb.store_bytes_batch(items, initial_lease).await,
            Backend::Sqlite(sqlite) => sqlite.store_bytes_batch(items, initial_lease).await,
        }
    }
}

// Benchmark: Single write operations
fn bench_store_bytes_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("store_bytes_single");

    // Test different blob sizes
    let sizes = vec![
        ("1kb", 1024),
        ("10kb", 10 * 1024),
        ("50kb", 50 * 1024),
        ("100kb", 100 * 1024),   // SQLite threshold
        ("500kb", 500 * 1024),
        ("1mb", 1024 * 1024),
        ("10mb", 10 * 1024 * 1024),
    ];

    for (name, size) in sizes {
        group.throughput(Throughput::Bytes(size as u64));

        // LMDB benchmark
        let tempdir = TempDir::new().unwrap();
        let backend = Backend::new_lmdb(tempdir.path(), 10 * 1024 * 1024 * 1024, 16); // 10GB
        let data = generate_data(size);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let counter = AtomicU64::new(0);

        group.bench_function(BenchmarkId::new("lmdb", name), |b| {
            b.iter(|| {
                let fp = generate_fingerprint(counter.fetch_add(1, Ordering::Relaxed));
                let data = data.clone();
                runtime.block_on(backend.store_bytes(fp, data, true)).unwrap()
            });
        });

        // SQLite benchmark
        let tempdir = TempDir::new().unwrap();
        let backend = Backend::new_sqlite(tempdir.path(), 1024 * 1024 * 1024);
        let data = generate_data(size);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let counter = AtomicU64::new(0);

        group.bench_function(BenchmarkId::new("sqlite", name), |b| {
            b.iter(|| {
                let fp = generate_fingerprint(counter.fetch_add(1, Ordering::Relaxed));
                let data = data.clone();
                runtime.block_on(backend.store_bytes(fp, data, true)).unwrap()
            });
        });
    }

    group.finish();
}

// Benchmark: Single read operations
fn bench_load_bytes_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("load_bytes_single");

    let sizes = vec![
        ("1kb", 1024),
        ("10kb", 10 * 1024),
        ("50kb", 50 * 1024),
        ("100kb", 100 * 1024),
        ("500kb", 500 * 1024),
        ("1mb", 1024 * 1024),
        ("10mb", 10 * 1024 * 1024),
    ];

    for (name, size) in sizes {
        group.throughput(Throughput::Bytes(size as u64));

        // LMDB benchmark
        let tempdir = TempDir::new().unwrap();
        let backend = Backend::new_lmdb(tempdir.path(), 10 * 1024 * 1024 * 1024, 16); // 10GB
        let runtime = tokio::runtime::Runtime::new().unwrap();

        // Pre-populate with data
        let data = generate_data(size);
        let fp = generate_fingerprint(0);
        runtime.block_on(backend.store_bytes(fp, data.clone(), true)).unwrap();

        group.bench_function(BenchmarkId::new("lmdb", name), |b| {
            b.iter(|| {
                runtime
                    .block_on(backend.load_bytes_with(fp, |bytes| Ok(Bytes::copy_from_slice(bytes))))
                    .unwrap()
            });
        });

        // SQLite benchmark
        let tempdir = TempDir::new().unwrap();
        let backend = Backend::new_sqlite(tempdir.path(), 1024 * 1024 * 1024);
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let data = generate_data(size);
        let fp = generate_fingerprint(0);
        runtime.block_on(backend.store_bytes(fp, data.clone(), true)).unwrap();

        group.bench_function(BenchmarkId::new("sqlite", name), |b| {
            b.iter(|| {
                runtime
                    .block_on(backend.load_bytes_with(fp, |bytes| Ok(Bytes::copy_from_slice(bytes))))
                    .unwrap()
            });
        });
    }

    group.finish();
}

// Benchmark: Batch write operations
fn bench_store_bytes_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("store_bytes_batch");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));

    let configs = vec![
        ("10x1kb", 10, 1024),
        ("100x1kb", 100, 1024),
        ("1000x1kb", 1000, 1024),
        ("10x100kb", 10, 100 * 1024),
        ("100x10kb", 100, 10 * 1024),
    ];

    for (name, count, size) in configs {
        let total_bytes = (count * size) as u64;
        group.throughput(Throughput::Bytes(total_bytes));

        // LMDB benchmark
        let tempdir = TempDir::new().unwrap();
        let backend = Backend::new_lmdb(tempdir.path(), 10 * 1024 * 1024 * 1024, 16); // 10GB
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let batch_counter = AtomicU64::new(0);

        group.bench_function(BenchmarkId::new("lmdb", name), |b| {
            b.iter(|| {
                let batch_id = batch_counter.fetch_add(1, Ordering::Relaxed);
                let items: Vec<_> = (0..count)
                    .map(|i| {
                        let fp = generate_fingerprint(batch_id * 10000 + i as u64);
                        let data = generate_data(size);
                        (fp, data)
                    })
                    .collect();
                runtime.block_on(backend.store_bytes_batch(items, true)).unwrap()
            });
        });

        // SQLite benchmark
        let tempdir = TempDir::new().unwrap();
        let backend = Backend::new_sqlite(tempdir.path(), 1024 * 1024 * 1024);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let batch_counter = AtomicU64::new(0);

        group.bench_function(BenchmarkId::new("sqlite", name), |b| {
            b.iter(|| {
                let batch_id = batch_counter.fetch_add(1, Ordering::Relaxed);
                let items: Vec<_> = (0..count)
                    .map(|i| {
                        let fp = generate_fingerprint(batch_id * 10000 + i as u64);
                        let data = generate_data(size);
                        (fp, data)
                    })
                    .collect();
                runtime.block_on(backend.store_bytes_batch(items, true)).unwrap()
            });
        });
    }

    group.finish();
}

// Benchmark: Batch exists operations
fn bench_exists_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("exists_batch");
    group.sample_size(20);

    let configs = vec![
        ("10_items_100pct_hit", 10, 100),
        ("100_items_100pct_hit", 100, 100),
        ("1000_items_100pct_hit", 1000, 100),
        ("100_items_50pct_hit", 100, 50),
        ("100_items_0pct_hit", 100, 0),
    ];

    for (name, count, hit_rate) in configs {
        // LMDB benchmark
        let tempdir = TempDir::new().unwrap();
        let backend = Backend::new_lmdb(tempdir.path(), 10 * 1024 * 1024 * 1024, 16); // 10GB
        let runtime = tokio::runtime::Runtime::new().unwrap();

        // Pre-populate based on hit rate
        let hit_count = (count * hit_rate) / 100;
        for i in 0..hit_count {
            let fp = generate_fingerprint(i as u64);
            let data = generate_data(1024);
            runtime.block_on(backend.store_bytes(fp, data, true)).unwrap();
        }

        let fingerprints: Vec<_> = (0..count).map(|i| generate_fingerprint(i as u64)).collect();

        group.bench_function(BenchmarkId::new("lmdb", name), |b| {
            b.iter(|| {
                let fps = fingerprints.clone();
                runtime.block_on(backend.exists_batch(fps)).unwrap()
            });
        });

        // SQLite benchmark
        let tempdir = TempDir::new().unwrap();
        let backend = Backend::new_sqlite(tempdir.path(), 1024 * 1024 * 1024);
        let runtime = tokio::runtime::Runtime::new().unwrap();

        for i in 0..hit_count {
            let fp = generate_fingerprint(i as u64);
            let data = generate_data(1024);
            runtime.block_on(backend.store_bytes(fp, data, true)).unwrap();
        }

        let fingerprints: Vec<_> = (0..count).map(|i| generate_fingerprint(i as u64)).collect();

        group.bench_function(BenchmarkId::new("sqlite", name), |b| {
            b.iter(|| {
                let fps = fingerprints.clone();
                runtime.block_on(backend.exists_batch(fps)).unwrap()
            });
        });
    }

    group.finish();
}

// Benchmark: Lease operations
fn bench_lease(c: &mut Criterion) {
    let mut group = c.benchmark_group("lease");
    group.sample_size(20);

    let counts = vec![10, 100, 1000];

    for count in counts {
        // LMDB benchmark
        let tempdir = TempDir::new().unwrap();
        let backend = Backend::new_lmdb(tempdir.path(), 10 * 1024 * 1024 * 1024, 16); // 10GB
        let runtime = tokio::runtime::Runtime::new().unwrap();

        // Pre-populate
        for i in 0..count {
            let fp = generate_fingerprint(i as u64);
            let data = generate_data(1024);
            runtime.block_on(backend.store_bytes(fp, data, true)).unwrap();
        }

        group.bench_function(BenchmarkId::new("lmdb", count), |b| {
            b.iter(|| {
                for i in 0..count {
                    let fp = generate_fingerprint(i as u64);
                    runtime.block_on(backend.lease(fp)).unwrap();
                }
            });
        });

        // SQLite benchmark
        let tempdir = TempDir::new().unwrap();
        let backend = Backend::new_sqlite(tempdir.path(), 1024 * 1024 * 1024);
        let runtime = tokio::runtime::Runtime::new().unwrap();

        for i in 0..count {
            let fp = generate_fingerprint(i as u64);
            let data = generate_data(1024);
            runtime.block_on(backend.store_bytes(fp, data, true)).unwrap();
        }

        group.bench_function(BenchmarkId::new("sqlite", count), |b| {
            b.iter(|| {
                for i in 0..count {
                    let fp = generate_fingerprint(i as u64);
                    runtime.block_on(backend.lease(fp)).unwrap();
                }
            });
        });
    }

    group.finish();
}

// Benchmark: Remove operations
fn bench_remove(c: &mut Criterion) {
    let mut group = c.benchmark_group("remove");
    group.sample_size(10);

    let configs = vec![
        ("small_blobs_10", 10, 1024),
        ("small_blobs_100", 100, 1024),
        ("large_blobs_10", 10, 200 * 1024), // Above SQLite threshold
    ];

    for (name, count, size) in configs {
        // LMDB benchmark
        group.bench_function(BenchmarkId::new("lmdb", name), |b| {
            b.iter_batched(
                || {
                    let tempdir = TempDir::new().unwrap();
                    let backend = Backend::new_lmdb(tempdir.path(), 10 * 1024 * 1024 * 1024, 16); // 10GB
                    let runtime = tokio::runtime::Runtime::new().unwrap();

                    // Pre-populate
                    for i in 0..count {
                        let fp = generate_fingerprint(i as u64);
                        let data = generate_data(size);
                        runtime.block_on(backend.store_bytes(fp, data, true)).unwrap();
                    }

                    (backend, tempdir)
                },
                |(backend, _tempdir)| {
                    let runtime = tokio::runtime::Runtime::new().unwrap();
                    for i in 0..count {
                        let fp = generate_fingerprint(i as u64);
                        runtime.block_on(backend.remove(fp)).unwrap();
                    }
                },
                criterion::BatchSize::SmallInput,
            );
        });

        // SQLite benchmark
        group.bench_function(BenchmarkId::new("sqlite", name), |b| {
            b.iter_batched(
                || {
                    let tempdir = TempDir::new().unwrap();
                    let backend = Backend::new_sqlite(tempdir.path(), 1024 * 1024 * 1024);
                    let runtime = tokio::runtime::Runtime::new().unwrap();

                    for i in 0..count {
                        let fp = generate_fingerprint(i as u64);
                        let data = generate_data(size);
                        runtime.block_on(backend.store_bytes(fp, data, true)).unwrap();
                    }

                    (backend, tempdir)
                },
                |(backend, _tempdir)| {
                    let runtime = tokio::runtime::Runtime::new().unwrap();
                    for i in 0..count {
                        let fp = generate_fingerprint(i as u64);
                        runtime.block_on(backend.remove(fp)).unwrap();
                    }
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_store_bytes_single,
    bench_load_bytes_single,
    bench_store_bytes_batch,
    bench_exists_batch,
    bench_lease,
    bench_remove,
);

fn main() {
    // Initialize Tokio runtime for the benchmarks
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();

    benches();
    Criterion::default().configure_from_args().final_summary();
}

