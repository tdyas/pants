// Copyright 2025 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

//! Practical benchmarks simulating realistic Pants build workflows.
//!
//! These benchmarks test end-to-end scenarios that mirror actual usage patterns,
//! including cold/warm cache behavior, incremental builds, and GC cycles.

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::Duration;
use task_executor::Executor;
use tempfile::TempDir;

use store::{LocalOptions, LocalStoreBackend, Store};

fn executor() -> Executor {
    Executor::new_owned(num_cpus::get(), num_cpus::get() * 4, || ()).unwrap()
}

// Test data generators
fn generate_data(size: usize) -> Bytes {
    Bytes::from(vec![42u8; size])
}

// Create a store with specified backend
fn create_store(backend: LocalStoreBackend, tempdir: &TempDir, immutable_inputs_base: &TempDir) -> Store {
    let executor = executor();
    let options = LocalOptions {
        files_max_size_bytes: 1024 * 1024 * 1024,       // 1GB
        directories_max_size_bytes: 1024 * 1024 * 1024, // 1GB
        lease_time: Duration::from_secs(2 * 60 * 60),   // 2 hours
        shard_count: 16,
        backend,
    };

    Store::local_only_with_options(executor, tempdir.path(), immutable_inputs_base.path(), options).unwrap()
}

// Benchmark: Cold cache build (initial population)
fn bench_cold_cache_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_cache_build");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    // Simulate storing files during initial build:
    // - 1000 source files (1-100KB each, realistic distribution)
    // - 500 intermediate artifacts (10KB-1MB)
    // - 100 final artifacts (1-10MB)

    let source_files: Vec<_> = (0..1000)
        .map(|i| {
            let size = if i < 800 {
                1024 + (i % 10) * 1024 // 1-10KB (80%)
            } else if i < 950 {
                10 * 1024 + (i % 10) * 10 * 1024 // 10-100KB (15%)
            } else {
                100 * 1024 + (i % 5) * 100 * 1024 // 100-500KB (5%)
            };
            generate_data(size)
        })
        .collect();

    let intermediate_artifacts: Vec<_> = (1000..1500)
        .map(|i| {
            let size = 10 * 1024 + (i % 100) * 10 * 1024; // 10KB-1MB
            generate_data(size)
        })
        .collect();

    let final_artifacts: Vec<_> = (1500..1600)
        .map(|i| {
            let size = 1024 * 1024 + (i % 10) * 1024 * 1024; // 1-10MB
            generate_data(size)
        })
        .collect();

    // LMDB benchmark
    group.bench_function(BenchmarkId::new("lmdb", "full_build"), |b| {
        b.iter_batched(
            || {
                let tempdir = TempDir::new().unwrap();
                let immutable_inputs_base = TempDir::new().unwrap();
                let store = create_store(LocalStoreBackend::Lmdb, &tempdir, &immutable_inputs_base);
                (store, tempdir, immutable_inputs_base)
            },
            |(store, _tempdir, _immutable_inputs_base)| {
                let runtime = tokio::runtime::Runtime::new().unwrap();

                // Store source files
                for data in &source_files {
                    runtime
                        .block_on(store.store_file_bytes(data.clone(), true))
                        .unwrap();
                }

                // Store intermediate artifacts
                for data in &intermediate_artifacts {
                    runtime
                        .block_on(store.store_file_bytes(data.clone(), true))
                        .unwrap();
                }

                // Store final artifacts
                for data in &final_artifacts {
                    runtime
                        .block_on(store.store_file_bytes(data.clone(), true))
                        .unwrap();
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // SQLite benchmark
    group.bench_function(BenchmarkId::new("sqlite", "full_build"), |b| {
        b.iter_batched(
            || {
                let tempdir = TempDir::new().unwrap();
                let immutable_inputs_base = TempDir::new().unwrap();
                let store = create_store(LocalStoreBackend::Sqlite, &tempdir, &immutable_inputs_base);
                (store, tempdir, immutable_inputs_base)
            },
            |(store, _tempdir, _immutable_inputs_base)| {
                let runtime = tokio::runtime::Runtime::new().unwrap();

                for data in &source_files {
                    runtime
                        .block_on(store.store_file_bytes(data.clone(), true))
                        .unwrap();
                }

                for data in &intermediate_artifacts {
                    runtime
                        .block_on(store.store_file_bytes(data.clone(), true))
                        .unwrap();
                }

                for data in &final_artifacts {
                    runtime
                        .block_on(store.store_file_bytes(data.clone(), true))
                        .unwrap();
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

// Benchmark: Warm cache build (mostly reads)
fn bench_warm_cache_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("warm_cache_build");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    // Simulate incremental build:
    // - Read 900 cached source files (90% hit rate)
    // - Store 100 new/modified source files
    // - Read 450 cached artifacts
    // - Store 50 new artifacts

    let all_files: Vec<_> = (0..1000)
        .map(|i| {
            let size = 1024 + (i % 100) * 1024; // 1-100KB
            generate_data(size)
        })
        .collect();

    let new_files: Vec<_> = (1000..1100)
        .map(|i| {
            let size = 1024 + (i % 100) * 1024;
            generate_data(size)
        })
        .collect();

    let all_artifacts: Vec<_> = (2000..2500)
        .map(|i| {
            let size = 100 * 1024 + (i % 10) * 100 * 1024; // 100KB-1MB
            generate_data(size)
        })
        .collect();

    let new_artifacts: Vec<_> = (2500..2550)
        .map(|i| {
            let size = 100 * 1024 + (i % 10) * 1024; // 100KB-1MB
            generate_data(size)
        })
        .collect();

    // LMDB benchmark
    group.bench_function(BenchmarkId::new("lmdb", "incremental_build"), |b| {
        b.iter_batched(
            || {
                let tempdir = TempDir::new().unwrap();
                let immutable_inputs_base = TempDir::new().unwrap();
                let store = create_store(LocalStoreBackend::Lmdb, &tempdir, &immutable_inputs_base);
                let runtime = tokio::runtime::Runtime::new().unwrap();

                // Pre-populate cache and collect digests
                let file_digests: Vec<_> = all_files
                    .iter()
                    .map(|data| {
                        runtime
                            .block_on(store.store_file_bytes(data.clone(), true))
                            .unwrap()
                    })
                    .collect();

                let artifact_digests: Vec<_> = all_artifacts
                    .iter()
                    .map(|data| {
                        runtime
                            .block_on(store.store_file_bytes(data.clone(), true))
                            .unwrap()
                    })
                    .collect();

                (store, tempdir, immutable_inputs_base, file_digests, artifact_digests)
            },
            |(store, _tempdir, _immutable_inputs_base, file_digests, artifact_digests)| {
                let runtime = tokio::runtime::Runtime::new().unwrap();

                // Read cached files (90%)
                for digest in file_digests.iter().take(900) {
                    runtime
                        .block_on(store.load_file_bytes_with(*digest, |bytes| bytes.len()))
                        .unwrap();
                }

                // Store new files (10%)
                for data in &new_files {
                    runtime
                        .block_on(store.store_file_bytes(data.clone(), true))
                        .unwrap();
                }

                // Read cached artifacts (90%)
                for digest in artifact_digests.iter().take(450) {
                    runtime
                        .block_on(store.load_file_bytes_with(*digest, |bytes| bytes.len()))
                        .unwrap();
                }

                // Store new artifacts (10%)
                for data in &new_artifacts {
                    runtime
                        .block_on(store.store_file_bytes(data.clone(), true))
                        .unwrap();
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // SQLite benchmark
    group.bench_function(BenchmarkId::new("sqlite", "incremental_build"), |b| {
        b.iter_batched(
            || {
                let tempdir = TempDir::new().unwrap();
                let immutable_inputs_base = TempDir::new().unwrap();
                let store = create_store(LocalStoreBackend::Sqlite, &tempdir, &immutable_inputs_base);
                let runtime = tokio::runtime::Runtime::new().unwrap();

                let file_digests: Vec<_> = all_files
                    .iter()
                    .map(|data| {
                        runtime
                            .block_on(store.store_file_bytes(data.clone(), true))
                            .unwrap()
                    })
                    .collect();

                let artifact_digests: Vec<_> = all_artifacts
                    .iter()
                    .map(|data| {
                        runtime
                            .block_on(store.store_file_bytes(data.clone(), true))
                            .unwrap()
                    })
                    .collect();

                (store, tempdir, immutable_inputs_base, file_digests, artifact_digests)
            },
            |(store, _tempdir, _immutable_inputs_base, file_digests, artifact_digests)| {
                let runtime = tokio::runtime::Runtime::new().unwrap();

                for digest in file_digests.iter().take(900) {
                    runtime
                        .block_on(store.load_file_bytes_with(*digest, |bytes| bytes.len()))
                        .unwrap();
                }

                for data in &new_files {
                    runtime
                        .block_on(store.store_file_bytes(data.clone(), true))
                        .unwrap();
                }

                for digest in artifact_digests.iter().take(450) {
                    runtime
                        .block_on(store.load_file_bytes_with(*digest, |bytes| bytes.len()))
                        .unwrap();
                }

                for data in &new_artifacts {
                    runtime
                        .block_on(store.store_file_bytes(data.clone(), true))
                        .unwrap();
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_cold_cache_build, bench_warm_cache_build,);
criterion_main!(benches);

