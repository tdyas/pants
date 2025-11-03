// Copyright 2025 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

use super::*;
use bytes::Bytes;
use hashing::Fingerprint;
use tempfile::TempDir;

fn new_store() -> (ShardedSqlite, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = ShardedSqlite::new(
        dir.path().to_path_buf(),
        1024 * 1024 * 1024, // 1GB
        DEFAULT_LEASE_TIME,
    )
    .unwrap();
    (store, dir)
}

#[tokio::test]
async fn test_store_and_load_small_blob() {
    let (store, _dir) = new_store();
    let data = Bytes::from("hello world");
    let fingerprint = Fingerprint::from_bytes_unsafe(b"12345678901234567890123456789012");

    store
        .store_bytes(fingerprint, data.clone(), false)
        .await
        .unwrap();

    let loaded = store
        .load_bytes_with(fingerprint, |bytes| Ok(Bytes::copy_from_slice(bytes)))
        .await
        .unwrap();

    assert_eq!(Some(data), loaded);
}

#[tokio::test]
async fn test_store_and_load_large_blob() {
    let (store, _dir) = new_store();
    // Create a blob larger than BLOB_SIZE_THRESHOLD (100KB)
    let data = Bytes::from(vec![42u8; 150 * 1024]);
    let fingerprint = Fingerprint::from_bytes_unsafe(b"12345678901234567890123456789012");

    store
        .store_bytes(fingerprint, data.clone(), false)
        .await
        .unwrap();

    let loaded = store
        .load_bytes_with(fingerprint, |bytes| Ok(Bytes::copy_from_slice(bytes)))
        .await
        .unwrap();

    assert_eq!(Some(data), loaded);
}

#[tokio::test]
async fn test_load_missing() {
    let (store, _dir) = new_store();
    let fingerprint = Fingerprint::from_bytes_unsafe(b"12345678901234567890123456789012");

    let loaded = store
        .load_bytes_with(fingerprint, |bytes| Ok(Bytes::copy_from_slice(bytes)))
        .await
        .unwrap();

    assert_eq!(None, loaded);
}

#[tokio::test]
async fn test_lease() {
    let (store, _dir) = new_store();
    let data = Bytes::from("test data");
    let fingerprint = Fingerprint::from_bytes_unsafe(b"12345678901234567890123456789012");

    // Store with initial lease
    store
        .store_bytes(fingerprint, data.clone(), true)
        .await
        .unwrap();

    // Extend lease
    store.lease(fingerprint).await.unwrap();

    // Check that fingerprint appears in all_fingerprints with no expiration
    let aged = store.all_fingerprints().await.unwrap();
    let found = aged.iter().find(|a| a.fingerprint == fingerprint);
    assert!(found.is_some());
    assert_eq!(found.unwrap().expired_seconds_ago, 0);
}

#[tokio::test]
async fn test_all_fingerprints() {
    let (store, _dir) = new_store();
    let data1 = Bytes::from("data1");
    let data2 = Bytes::from("data2");
    let fp1 = Fingerprint::from_bytes_unsafe(b"11111111111111111111111111111111");
    let fp2 = Fingerprint::from_bytes_unsafe(b"22222222222222222222222222222222");

    store.store_bytes(fp1, data1, false).await.unwrap();
    store.store_bytes(fp2, data2, false).await.unwrap();

    let aged = store.all_fingerprints().await.unwrap();
    assert_eq!(aged.len(), 2);

    let fps: Vec<Fingerprint> = aged.iter().map(|a| a.fingerprint).collect();
    assert!(fps.contains(&fp1));
    assert!(fps.contains(&fp2));
}

#[tokio::test]
async fn test_remove() {
    let (store, _dir) = new_store();
    let data = Bytes::from("test data");
    let fingerprint = Fingerprint::from_bytes_unsafe(b"12345678901234567890123456789012");

    store
        .store_bytes(fingerprint, data.clone(), false)
        .await
        .unwrap();

    assert!(store.exists(fingerprint).await.unwrap());

    let removed = store.remove(fingerprint).await.unwrap();
    assert!(removed);

    assert!(!store.exists(fingerprint).await.unwrap());
}

#[tokio::test]
async fn test_remove_large_blob() {
    let (store, _dir) = new_store();
    // Create a blob larger than BLOB_SIZE_THRESHOLD (100KB)
    let data = Bytes::from(vec![42u8; 150 * 1024]);
    let fingerprint = Fingerprint::from_bytes_unsafe(b"12345678901234567890123456789012");

    store
        .store_bytes(fingerprint, data.clone(), false)
        .await
        .unwrap();

    assert!(store.exists(fingerprint).await.unwrap());

    // Verify blob file exists
    let blob_path = store.blob_path(&fingerprint);
    assert!(blob_path.exists());

    let removed = store.remove(fingerprint).await.unwrap();
    assert!(removed);

    assert!(!store.exists(fingerprint).await.unwrap());
    assert!(!blob_path.exists());
}

#[tokio::test]
async fn test_exists() {
    let (store, _dir) = new_store();
    let data = Bytes::from("test data");
    let fingerprint = Fingerprint::from_bytes_unsafe(b"12345678901234567890123456789012");

    assert!(!store.exists(fingerprint).await.unwrap());

    store
        .store_bytes(fingerprint, data.clone(), false)
        .await
        .unwrap();

    assert!(store.exists(fingerprint).await.unwrap());
}

