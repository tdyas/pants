# Benchmarking SQLite vs LMDB Local Store Backends

This document describes the benchmarks available for comparing the performance of SQLite and LMDB backends for Pants' local cache storage.

## ⚠️ Current Status

**Note:** The micro-benchmarks in `sharded_sqlite/benches/backend_comparison.rs` are currently experiencing runtime issues with the Executor initialization. The benchmarks compile successfully and are properly defined (66 benchmarks total), but hang when executed. This appears to be related to the Tokio runtime context required by `Executor::new_owned()`.

**Working benchmarks:**
- ✅ Build simulation benchmarks (`fs/store/benches/build_simulation.rs`) - Compile successfully
- ✅ Existing store benchmarks (`fs/store/benches/store.rs`) - Work with LMDB backend

**Needs fixing:**
- ⚠️ Backend comparison micro-benchmarks - Hang at runtime due to Executor/Tokio runtime issues

## Overview

We have implemented three types of benchmarks to evaluate backend performance:

1. **Micro-benchmarks** (`sharded_sqlite/benches/backend_comparison.rs`) - Direct comparison of low-level backend operations (66 benchmarks defined)
2. **Practical benchmarks** (`fs/store/benches/build_simulation.rs`) - Realistic build workflow simulations
3. **Existing benchmarks** (`fs/store/benches/store.rs`) - High-level Store operations (now backend-aware)

## Running Benchmarks

### Micro-Benchmarks (Backend Comparison)

These benchmarks test the `UnderlyingByteStore` trait methods directly to isolate backend performance:

```bash
cd src/rust/sharded_sqlite
cargo bench --bench backend_comparison
```

**What it tests:**
- `store_bytes_single` - Single blob write operations (1KB to 10MB)
- `load_bytes_single` - Single blob read operations (1KB to 10MB)
- `store_bytes_batch` - Batch write operations (10-1000 items)
- `exists_batch` - Batch existence checks with varying hit rates
- `lease` - Lease update operations (10-1000 items)
- `remove` - Delete operations for small and large blobs

**Key insights:**
- Performance across the 100KB threshold (SQLite's in-DB vs on-disk decision)
- Batch operation efficiency
- Read vs write performance characteristics

### Practical Benchmarks (Build Simulation)

These benchmarks simulate realistic Pants build workflows:

```bash
cd src/rust/fs/store
cargo bench --bench build_simulation
```

**What it tests:**
- `cold_cache_build` - Initial build with empty cache (1600 files stored)
  - 1000 source files (1-500KB, realistic distribution)
  - 500 intermediate artifacts (10KB-1MB)
  - 100 final artifacts (1-10MB)
  
- `warm_cache_build` - Incremental build with warm cache
  - 90% cache hits (reads)
  - 10% new/modified files (writes)

**Key insights:**
- End-to-end performance in realistic scenarios
- Cache hit/miss patterns
- Mixed read/write workload behavior

### Existing Store Benchmarks

The existing high-level benchmarks now support backend selection:

```bash
cd src/rust/fs/store
cargo bench --bench store
```

**What it tests:**
- `materialize_directory` - Extract files from store to disk
- `snapshot_capture` - Digest and store files
- `digest_subset` - Extract subset using globs
- `snapshot_merge` - Merge snapshots

**Note:** These benchmarks currently use LMDB by default. To test with SQLite, you would need to modify the `snapshot()` function to use `LocalStoreBackend::Sqlite`.

## Running Specific Benchmarks

You can run specific benchmark functions:

```bash
# Run only single write benchmarks
cargo bench --bench backend_comparison -- store_bytes_single

# Run only 1KB write benchmarks
cargo bench --bench backend_comparison -- store_bytes_single/1kb

# Run only LMDB benchmarks
cargo bench --bench backend_comparison -- lmdb

# Run only SQLite benchmarks
cargo bench --bench backend_comparison -- sqlite
```

## Benchmark Output

Criterion generates HTML reports in `target/criterion/`:

```
target/criterion/
├── store_bytes_single/
│   ├── 1kb/
│   │   ├── lmdb/report/index.html
│   │   └── sqlite/report/index.html
│   ├── 10kb/
│   │   ├── lmdb/report/index.html
│   │   └── sqlite/report/index.html
│   └── ...
├── cold_cache_build/
│   ├── lmdb/report/index.html
│   └── sqlite/report/index.html
└── ...
```

Open these HTML files in a browser to see detailed performance charts, including:
- Mean execution time with confidence intervals
- Throughput (operations/sec or MB/sec)
- Performance trends over time (if run multiple times)

## Interpreting Results

### Key Metrics to Compare

1. **Throughput** - Operations per second or MB/sec
   - Higher is better
   - Look for differences across blob sizes

2. **Latency** - Time per operation
   - Lower is better
   - Check p50, p95, p99 percentiles in detailed reports

3. **Scalability** - Performance vs database size
   - How does performance degrade with more entries?

4. **Concurrency** - Performance with multiple threads
   - Currently tested implicitly through batch operations

### Expected Differences

Based on the design:

**SQLite may be faster for:**
- Large blobs (>100KB) - stored on disk, not in database
- Sequential scans - SQL indexes
- Garbage collection - SQL queries vs iteration

**LMDB may be faster for:**
- Small blobs (<100KB) - memory-mapped, no SQL overhead
- Write concurrency - multiple shards enable parallel writes
- Simple lookups - direct memory access

**Critical threshold:**
- 100KB - SQLite switches from in-DB to on-disk storage
- 512KB - LMDB switches to FSDB storage

## Customizing Benchmarks

### Adjusting Sample Size and Duration

Edit the benchmark files to change Criterion settings:

```rust
group
    .sample_size(10)                              // Number of iterations
    .measurement_time(Duration::from_secs(30))    // Time per benchmark
    .warm_up_time(Duration::from_secs(3));        // Warmup period
```

### Testing Different Configurations

Modify backend creation parameters:

```rust
// Test different LMDB shard counts
Backend::new_lmdb(path, max_size, 1);   // Single shard
Backend::new_lmdb(path, max_size, 16);  // Default
Backend::new_lmdb(path, max_size, 64);  // High concurrency

// Test different SQLite configurations
// (Would require exposing PRAGMA settings)
```

### Adding New Benchmarks

Follow the existing patterns:

```rust
fn bench_my_operation(c: &mut Criterion) {
    let mut group = c.benchmark_group("my_operation");
    
    // LMDB benchmark
    let tempdir = TempDir::new().unwrap();
    let backend = Backend::new_lmdb(tempdir.path(), 1024 * 1024 * 1024, 16);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    
    group.bench_function(BenchmarkId::new("lmdb", "test"), |b| {
        b.iter(|| {
            runtime.block_on(backend.some_operation()).unwrap()
        });
    });
    
    // SQLite benchmark
    let tempdir = TempDir::new().unwrap();
    let backend = Backend::new_sqlite(tempdir.path(), 1024 * 1024 * 1024);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    
    group.bench_function(BenchmarkId::new("sqlite", "test"), |b| {
        b.iter(|| {
            runtime.block_on(backend.some_operation()).unwrap()
        });
    });
    
    group.finish();
}

// Add to criterion_group!
criterion_group!(benches, bench_my_operation);
```

## Continuous Benchmarking

To track performance over time:

1. **Baseline**: Run benchmarks and save results
   ```bash
   cargo bench --bench backend_comparison -- --save-baseline main
   ```

2. **Compare**: After changes, compare against baseline
   ```bash
   cargo bench --bench backend_comparison -- --baseline main
   ```

3. **CI Integration**: Consider adding benchmark runs to CI
   - Run on performance-critical PRs
   - Track regressions
   - Store historical data

## Troubleshooting

### Backend comparison benchmarks hang at runtime

**Current Issue:** The `backend_comparison` benchmarks hang after compilation when trying to execute.

**Root Cause:** The `Executor::new_owned()` call in the LMDB backend initialization requires a Tokio runtime context, but Criterion benchmarks don't automatically provide one.

**Attempted Solutions:**
- ✅ Fixed async benchmark syntax (removed `to_async()` calls, use `block_on()` instead)
- ✅ Fixed mutable counter issues (use `AtomicU64` instead)
- ⚠️ Still investigating Executor initialization issue

**Potential Fixes:**
1. Wrap the entire benchmark in a Tokio runtime:
   ```rust
   let runtime = tokio::runtime::Runtime::new().unwrap();
   runtime.block_on(async {
       // benchmark code here
   });
   ```

2. Use a global runtime initialized once:
   ```rust
   lazy_static! {
       static ref RUNTIME: tokio::runtime::Runtime =
           tokio::runtime::Runtime::new().unwrap();
   }
   ```

3. Modify `Executor::new_owned()` to not require a Tokio context, or create a simpler executor for benchmarks

**Workaround:** Use the build simulation benchmarks and existing store benchmarks instead, which work correctly.

### Benchmarks are too slow

- Reduce `sample_size` (default: 100)
- Reduce `measurement_time` (default: 5 seconds)
- Run specific benchmarks instead of all

### Results are noisy

- Close other applications
- Disable CPU frequency scaling
- Increase `sample_size` for more stable results
- Run on dedicated benchmark hardware

### Out of disk space

- Benchmarks create many temporary directories
- Clean up `/tmp` or set `TMPDIR` to a location with more space
- Reduce the number of files in build simulation benchmarks

## Next Steps

After running benchmarks:

1. **Analyze results** - Compare HTML reports
2. **Identify bottlenecks** - Which operations are slower?
3. **Profile if needed** - Use `perf`, `flamegraph`, or other profilers
4. **Optimize** - Focus on the most impactful operations
5. **Re-benchmark** - Verify improvements

## References

- [Criterion.rs Documentation](https://bheisler.github.io/criterion.rs/book/)
- [SQLite Performance Tuning](https://www.sqlite.org/pragma.html)
- [LMDB Documentation](http://www.lmdb.tech/doc/)

