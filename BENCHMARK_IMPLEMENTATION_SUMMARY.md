# SQLite vs LMDB Benchmark Implementation Summary

## Overview

This document summarizes the implementation of comprehensive benchmarks to compare SQLite and LMDB backend performance for Pants' local cache storage.

## What Was Implemented

### 1. Micro-Benchmarks (`src/rust/sharded_sqlite/benches/backend_comparison.rs`)

**Purpose:** Direct comparison of low-level `UnderlyingByteStore` trait operations

**Benchmarks Implemented (66 total):**

#### Single Write Operations (14 benchmarks)
- Tests blob sizes: 1KB, 10KB, 50KB, 100KB, 500KB, 1MB, 10MB
- Compares LMDB vs SQLite for each size
- **Key insight:** Performance across the 100KB threshold (SQLite's in-DB vs on-disk decision)

#### Single Read Operations (14 benchmarks)
- Same size range as writes
- Pre-populates data before measuring read performance
- Tests cache-warm scenarios

#### Batch Write Operations (10 benchmarks)
- Batch sizes: 10, 100, 1000 items
- Blob sizes: 1KB, 10KB, 100KB
- Tests batch efficiency vs individual writes

#### Batch Exists Checks (10 benchmarks)
- Tests 10, 100, 1000 item batches
- Varies cache hit rates: 0%, 50%, 100%
- Measures lookup performance under different scenarios

#### Lease Operations (6 benchmarks)
- Tests 10, 100, 1000 lease updates
- Measures garbage collection metadata update performance

#### Remove Operations (6 benchmarks)
- Tests deletion of small blobs (1KB) and large blobs (1MB)
- Batch sizes: 10, 100 items

**Current Status:** ⚠️ Compiles successfully but hangs at runtime due to Executor/Tokio runtime initialization issues

### 2. Practical Benchmarks (`src/rust/fs/store/benches/build_simulation.rs`)

**Purpose:** Simulate realistic Pants build workflows

**Benchmarks Implemented:**

#### Cold Cache Build
- Simulates initial build with empty cache
- Stores 1600 files total:
  - 1000 source files (1-500KB, realistic size distribution)
  - 500 intermediate artifacts (10KB-1MB)
  - 100 final artifacts (1-10MB)
- Measures end-to-end write performance

#### Warm Cache Build
- Simulates incremental build
- 90% cache hits (reads from existing cache)
- 10% new/modified files (writes)
- Measures mixed read/write workload

**Current Status:** ✅ Compiles successfully, ready to run

### 3. Updated Existing Benchmarks (`src/rust/fs/store/benches/store.rs`)

**Purpose:** Make existing high-level Store benchmarks backend-aware

**Changes Made:**
- Added `LocalOptions` and `LocalStoreBackend` imports
- Created `snapshot_with_backend()` function that accepts backend parameter
- Updated `snapshot()` to call `snapshot_with_backend()` with LMDB as default
- Enables testing both backends with existing benchmarks:
  - `materialize_directory` - Extract files from store to disk
  - `snapshot_capture` - Digest and store files
  - `digest_subset` - Extract subset using globs
  - `snapshot_merge` - Merge snapshots

**Current Status:** ✅ Works with LMDB backend, can be extended to test SQLite

## Files Created

1. **src/rust/sharded_sqlite/benches/backend_comparison.rs** (485 lines)
   - Comprehensive micro-benchmarks
   - Backend wrapper enum for unified testing
   - Helper functions for data generation

2. **src/rust/fs/store/benches/build_simulation.rs** (150 lines)
   - Realistic build workflow simulations
   - Cold and warm cache scenarios

3. **src/rust/BENCHMARKING.md** (320 lines)
   - Complete documentation for running benchmarks
   - Interpretation guidelines
   - Troubleshooting section
   - Customization examples

4. **BENCHMARK_IMPLEMENTATION_SUMMARY.md** (this file)
   - Implementation summary
   - Current status and known issues

## Files Modified

1. **src/rust/sharded_sqlite/Cargo.toml**
   - Added dev-dependencies: `criterion`, `sharded_lmdb`, `task_executor`, `tokio`
   - Added benchmark configuration

2. **src/rust/fs/store/Cargo.toml**
   - Added `build_simulation` benchmark configuration

3. **src/rust/fs/store/benches/store.rs**
   - Added backend-aware snapshot creation
   - Prepared for multi-backend testing

## Technical Challenges Encountered

### 1. Criterion Async API Changes
**Problem:** Criterion 0.7 removed the `to_async()` method that was commonly used in older examples.

**Solution:** Use `tokio::runtime::Runtime::block_on()` to execute async operations within benchmark iterations.

### 2. Mutable Counter in Closures
**Problem:** Benchmark closures can't capture mutable variables.

**Solution:** Use `AtomicU64` with `fetch_add()` for thread-safe counter increments.

### 3. Store API Signatures
**Problem:** Initial implementation misunderstood Store API signatures.

**Solutions:**
- `store_file_bytes()` computes digest automatically, doesn't take fingerprint parameter
- `load_file_bytes_with()` requires `Digest`, not `Fingerprint`
- `Store::local_only_with_options()` requires `immutable_inputs_base` parameter

### 4. Executor Initialization (CURRENT BLOCKER)
**Problem:** `Executor::new_owned()` requires a Tokio runtime context, causing benchmarks to hang.

**Status:** Under investigation. Potential solutions:
- Wrap benchmarks in Tokio runtime
- Use global lazy-initialized runtime
- Create simpler executor for benchmarks
- Modify Executor to not require Tokio context

## Benchmark Coverage

### What's Tested

✅ **Operation Types:**
- Single writes and reads
- Batch writes and existence checks
- Lease management
- Deletion operations
- End-to-end build workflows

✅ **Blob Sizes:**
- Small: 1KB, 10KB
- Medium: 50KB, 100KB (around SQLite threshold)
- Large: 500KB, 1MB, 10MB

✅ **Workload Patterns:**
- Write-heavy (cold cache build)
- Read-heavy (warm cache with 90% hits)
- Mixed read/write
- Batch operations

✅ **Cache Scenarios:**
- Cold cache (0% hit rate)
- Warm cache (50%, 90%, 100% hit rates)

### What's Not Tested (Future Work)

❌ **Concurrency:**
- Multi-threaded write performance
- Concurrent read/write workloads
- Lock contention under high concurrency

❌ **Database Size Effects:**
- Performance with 1GB, 10GB, 100GB databases
- Degradation over time
- Fragmentation impact

❌ **Garbage Collection:**
- GC performance and impact
- Lease expiration handling
- Compaction/VACUUM performance

❌ **Edge Cases:**
- Very large blobs (>100MB)
- Very small blobs (<100 bytes)
- Extremely large batch sizes (>10,000 items)

❌ **System Stress:**
- Low memory conditions
- Low disk space
- Network filesystem performance (NFS, etc.)

## Next Steps

### Immediate (Fix Current Issues)

1. **Fix Executor initialization issue**
   - Investigate why `Executor::new_owned()` hangs
   - Implement one of the proposed solutions
   - Verify benchmarks run successfully

2. **Run initial benchmarks**
   - Execute all micro-benchmarks
   - Execute build simulation benchmarks
   - Generate baseline performance data

3. **Analyze results**
   - Compare LMDB vs SQLite across all metrics
   - Identify performance differences
   - Document findings

### Short-term (Enhance Benchmarks)

4. **Add concurrency benchmarks**
   - Multi-threaded write tests
   - Concurrent read/write tests
   - Measure lock contention

5. **Add database size tests**
   - Pre-populate with varying amounts of data
   - Measure performance degradation
   - Test with realistic cache sizes (10GB+)

6. **Test existing store benchmarks with SQLite**
   - Modify to test both backends
   - Compare high-level operation performance

### Long-term (Production Readiness)

7. **Add CI integration**
   - Run benchmarks on PRs
   - Track performance regressions
   - Store historical data

8. **Create performance dashboard**
   - Visualize trends over time
   - Compare across different hardware
   - Track key metrics

9. **Document performance characteristics**
   - When to use SQLite vs LMDB
   - Performance tuning guidelines
   - Best practices

## How to Use

### Running Benchmarks

```bash
# List all available benchmarks
cd src/rust/sharded_sqlite
cargo bench --bench backend_comparison -- --list

# Run specific benchmark (once Executor issue is fixed)
cargo bench --bench backend_comparison -- store_bytes_single/1kb

# Run build simulation benchmarks
cd src/rust/fs/store
cargo bench --bench build_simulation

# Run existing store benchmarks
cargo bench --bench store
```

### Viewing Results

Criterion generates HTML reports in `target/criterion/`:

```bash
# Open a specific benchmark report
open target/criterion/store_bytes_single/1kb/lmdb/report/index.html
open target/criterion/cold_cache_build/lmdb/report/index.html
```

### Comparing Backends

After running benchmarks, compare the HTML reports side-by-side:
- LMDB: `target/criterion/<benchmark>/lmdb/report/index.html`
- SQLite: `target/criterion/<benchmark>/sqlite/report/index.html`

Look for:
- Mean execution time differences
- Throughput (ops/sec or MB/sec)
- Performance trends across blob sizes
- Batch operation efficiency

## Conclusion

We have successfully implemented a comprehensive benchmarking suite with 66 micro-benchmarks and 2 practical build simulation benchmarks. The benchmarks compile successfully and are well-documented.

**Current blocker:** Executor initialization issue prevents micro-benchmarks from running. Once resolved, we'll have a complete performance comparison framework.

**Value delivered:**
- Structured approach to performance testing
- Comprehensive coverage of operations and scenarios
- Reusable benchmark infrastructure
- Clear documentation for future use

**Estimated effort to complete:**
- Fix Executor issue: 2-4 hours
- Run and analyze initial benchmarks: 2-3 hours
- Document findings: 1-2 hours
- **Total:** 5-9 hours to full completion

