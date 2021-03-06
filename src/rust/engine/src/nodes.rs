// Copyright 2018 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

use std::collections::{BTreeMap, HashMap};
use std::convert::TryFrom;
use std::fmt::Display;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{self, fmt};

use async_trait::async_trait;
use futures::compat::Future01CompatExt;
use futures::future::{FutureExt, TryFutureExt};
use futures::stream::StreamExt;
use futures01::future::{self, Future};
use url::Url;

use crate::context::{Context, Core};
use crate::core::{throw, Failure, Key, Params, TypeId, Value};
use crate::externs;
use crate::selectors;
use crate::tasks::{self, Rule};
use boxfuture::{try_future, BoxFuture, Boxable};
use bytes::{self, BufMut};
use fs::{
  self, Dir, DirectoryListing, File, FileContent, GlobExpansionConjunction, GlobMatching, Link,
  PathGlobs, PathStat, PreparedPathGlobs, StrictGlobMatching, VFS,
};
use hashing;
use process_execution::{self, MultiPlatformProcess, PlatformConstraint, Process, RelativePath};
use rule_graph;

use graph::{Entry, Node, NodeError, NodeTracer, NodeVisualizer};
use store::{self, StoreFileByDigest};
use workunit_store::{new_span_id, scope_task_workunit_state, WorkunitMetadata};

pub type NodeFuture<T> = BoxFuture<T, Failure>;

fn ok<O: Send + 'static>(value: O) -> NodeFuture<O> {
  future::ok(value).to_boxed()
}

fn err<O: Send + 'static>(failure: Failure) -> NodeFuture<O> {
  future::err(failure).to_boxed()
}

#[async_trait]
impl VFS<Failure> for Context {
  async fn read_link(&self, link: &Link) -> Result<PathBuf, Failure> {
    Ok(self.get(ReadLink(link.clone())).compat().await?.0)
  }

  async fn scandir(&self, dir: Dir) -> Result<Arc<DirectoryListing>, Failure> {
    self.get(Scandir(dir)).compat().await
  }

  fn is_ignored(&self, stat: &fs::Stat) -> bool {
    self.core.vfs.is_ignored(stat)
  }

  fn mk_error(msg: &str) -> Failure {
    Failure::Throw(
      externs::create_exception(msg),
      "<pants native internals>".to_string(),
    )
  }
}

impl StoreFileByDigest<Failure> for Context {
  fn store_by_digest(&self, file: File) -> BoxFuture<hashing::Digest, Failure> {
    self.get(DigestFile(file))
  }
}

///
/// A simplified implementation of graph::Node for members of the NodeKey enum to implement.
/// NodeKey's impl of graph::Node handles the rest.
///
/// The Item type of a WrappedNode is bounded to values that can be stored and retrieved
/// from the NodeResult enum. Due to the semantics of memoization, retrieving the typed result
/// stored inside the NodeResult requires an implementation of TryFrom<NodeResult>. But the
/// combination of bounds at usage sites should mean that a failure to unwrap the result is
/// exceedingly rare.
///
pub trait WrappedNode: Into<NodeKey> {
  type Item: TryFrom<NodeResult>;

  fn run(self, context: Context) -> BoxFuture<Self::Item, Failure>;
}

///
/// A Node that selects a product for some Params.
///
/// A Select can be satisfied by multiple sources, but fails if multiple sources produce a value.
/// The 'params' represent a series of type-keyed parameters that will be used by Nodes in the
/// subgraph below this Select.
///
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Select {
  pub params: Params,
  pub product: TypeId,
  entry: rule_graph::Entry<Rule>,
}

impl Select {
  pub fn new(mut params: Params, product: TypeId, entry: rule_graph::Entry<Rule>) -> Select {
    params.retain(|k| match &entry {
      &rule_graph::Entry::Param(ref type_id) => type_id == k.type_id(),
      &rule_graph::Entry::WithDeps(ref with_deps) => with_deps.params().contains(k.type_id()),
    });
    Select {
      params,
      product,
      entry,
    }
  }

  pub fn new_from_edges(
    params: Params,
    product: TypeId,
    edges: &rule_graph::RuleEdges<Rule>,
  ) -> Select {
    let dependency_key = selectors::DependencyKey::JustSelect(selectors::Select::new(product));
    // TODO: Is it worth propagating an error here?
    let entry = edges
      .entry_for(&dependency_key)
      .unwrap_or_else(|| panic!("{:?} did not declare a dependency on {:?}", edges, product))
      .clone();
    Select::new(params, product, entry)
  }

  fn select_product(
    &self,
    context: &Context,
    product: TypeId,
    caller_description: &str,
  ) -> NodeFuture<Value> {
    let edges = context
      .core
      .rule_graph
      .edges_for_inner(&self.entry)
      .ok_or_else(|| {
        throw(&format!(
          "Tried to select product {} for {} but found no edges",
          product, caller_description
        ))
      });
    let context = context.clone();
    Select::new_from_edges(self.params.clone(), product, &try_future!(edges)).run(context)
  }
}

// TODO: This is a Node only because it is used as a root in the graph, but it should never be
// requested using context.get
impl WrappedNode for Select {
  type Item = Value;

  fn run(self, context: Context) -> NodeFuture<Value> {
    match &self.entry {
      &rule_graph::Entry::WithDeps(rule_graph::EntryWithDeps::Inner(ref inner)) => {
        match inner.rule() {
          &tasks::Rule::Task(ref task) => context.get(Task {
            params: self.params.clone(),
            product: self.product,
            task: task.clone(),
            entry: Arc::new(self.entry.clone()),
          }),
          &Rule::Intrinsic(ref intrinsic) => {
            let intrinsic = intrinsic.clone();
            future::join_all(
              intrinsic
                .inputs
                .iter()
                .map(|type_id| self.select_product(&context, *type_id, "intrinsic"))
                .collect::<Vec<_>>(),
            )
            .and_then(move |values| {
              let core = context.core.clone();
              core.intrinsics.run(intrinsic, context, values)
            })
            .to_boxed()
          }
        }
      }
      &rule_graph::Entry::Param(type_id) => {
        if let Some(key) = self.params.find(type_id) {
          ok(externs::val_for(key))
        } else {
          err(throw(&format!(
            "Expected a Param of type {} to be present.",
            type_id
          )))
        }
      }
      &rule_graph::Entry::WithDeps(rule_graph::EntryWithDeps::Root(_)) => {
        panic!("Not a runtime-executable entry! {:?}", self.entry)
      }
    }
  }
}

impl From<Select> for NodeKey {
  fn from(n: Select) -> Self {
    NodeKey::Select(Box::new(n))
  }
}

pub fn lift_digest(digest: &Value) -> Result<hashing::Digest, String> {
  let fingerprint = externs::project_str(&digest, "fingerprint");
  let digest_length = externs::project_str(&digest, "serialized_bytes_length");
  let digest_length_as_usize = digest_length
    .parse::<usize>()
    .map_err(|err| format!("Length was not a usize: {:?}", err))?;
  Ok(hashing::Digest(
    hashing::Fingerprint::from_hex_string(&fingerprint)?,
    digest_length_as_usize,
  ))
}

/// A Node that represents a set of processes to execute on specific platforms.
///
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MultiPlatformExecuteProcess(MultiPlatformProcess);

impl MultiPlatformExecuteProcess {
  fn lift_execute_process(
    value: &Value,
    target_platform: PlatformConstraint,
  ) -> Result<Process, String> {
    let env = externs::project_tuple_encoded_map(&value, "env")?;

    let working_directory = {
      let val = externs::project_str(&value, "working_directory");
      if val.is_empty() {
        None
      } else {
        Some(RelativePath::new(val.as_str())?)
      }
    };

    let digest = lift_digest(&externs::project_ignoring_type(&value, "input_digest"))
      .map_err(|err| format!("Error parsing digest {}", err))?;

    let output_files = externs::project_multi_strs(&value, "output_files")
      .into_iter()
      .map(PathBuf::from)
      .collect();

    let output_directories = externs::project_multi_strs(&value, "output_directories")
      .into_iter()
      .map(PathBuf::from)
      .collect();

    let timeout_str = externs::project_str(&value, "timeout_seconds");
    let timeout_in_seconds = timeout_str
      .parse::<f64>()
      .map_err(|err| format!("Timeout was not a float: {:?}", err))?;

    let timeout = if timeout_in_seconds < 0.0 {
      None
    } else {
      Some(Duration::from_millis((timeout_in_seconds * 1000.0) as u64))
    };

    let description = externs::project_str(&value, "description");

    let jdk_home = {
      let val = externs::project_str(&value, "jdk_home");
      if val.is_empty() {
        None
      } else {
        Some(PathBuf::from(val))
      }
    };

    let is_nailgunnable = externs::project_bool(&value, "is_nailgunnable");

    let unsafe_local_only_files_because_we_favor_speed_over_correctness_for_this_rule =
      lift_digest(&externs::project_ignoring_type(
        &value,
        "unsafe_local_only_files_because_we_favor_speed_over_correctness_for_this_rule",
      ))
      .map_err(|err| format!("Error parsing digest {}", err))?;

    Ok(process_execution::Process {
      argv: externs::project_multi_strs(&value, "argv"),
      env,
      working_directory,
      input_files: digest,
      output_files,
      output_directories,
      timeout,
      description,
      unsafe_local_only_files_because_we_favor_speed_over_correctness_for_this_rule,
      jdk_home,
      target_platform,
      is_nailgunnable,
    })
  }

  pub fn lift(value: &Value) -> Result<MultiPlatformExecuteProcess, String> {
    let constraint_parts = externs::project_multi_strs(&value, "platform_constraints");
    if constraint_parts.len() % 2 != 0 {
      return Err("Error parsing platform_constraints: odd number of parts".to_owned());
    }
    let constraint_key_pairs: Vec<_> = constraint_parts
      .chunks_exact(2)
      .map(|constraint_key_pair| {
        (
          PlatformConstraint::try_from(&constraint_key_pair[0]).unwrap(),
          PlatformConstraint::try_from(&constraint_key_pair[1]).unwrap(),
        )
      })
      .collect();
    let processes = externs::project_multi(&value, "processes");
    if constraint_parts.len() / 2 != processes.len() {
      return Err(format!(
        "Sizes of constraint keys and processes do not match: {} vs. {}",
        constraint_parts.len() / 2,
        processes.len()
      ));
    }

    let mut request_by_constraint: BTreeMap<(PlatformConstraint, PlatformConstraint), Process> =
      BTreeMap::new();
    for (constraint_key, execute_process) in constraint_key_pairs.iter().zip(processes.iter()) {
      let underlying_req =
        MultiPlatformExecuteProcess::lift_execute_process(execute_process, constraint_key.1)?;
      request_by_constraint.insert(constraint_key.clone(), underlying_req.clone());
    }
    Ok(MultiPlatformExecuteProcess(MultiPlatformProcess(
      request_by_constraint,
    )))
  }
}

impl From<MultiPlatformExecuteProcess> for NodeKey {
  fn from(n: MultiPlatformExecuteProcess) -> Self {
    NodeKey::MultiPlatformExecuteProcess(Box::new(n))
  }
}

impl WrappedNode for MultiPlatformExecuteProcess {
  type Item = ProcessResult;

  fn run(self, context: Context) -> NodeFuture<ProcessResult> {
    let request = self.0;
    let execution_context = process_execution::Context::new(
      context.session.workunit_store(),
      context.session.build_id().to_string(),
    );
    if context
      .core
      .command_runner
      .extract_compatible_request(&request)
      .is_some()
    {
      Box::pin(async move {
        let res = context
          .core
          .command_runner
          .run(request, execution_context)
          .await
          .map_err(|e| throw(&format!("Failed to execute process: {}", e)))?;

        Ok(ProcessResult(res))
      })
      .compat()
      .to_boxed()
    } else {
      err(throw(&format!(
        "No compatible platform found for request: {:?}",
        request
      )))
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessResult(pub process_execution::FallibleProcessResultWithPlatform);

///
/// A Node that represents reading the destination of a symlink (non-recursively).
///
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReadLink(Link);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkDest(PathBuf);

impl WrappedNode for ReadLink {
  type Item = LinkDest;

  fn run(self, context: Context) -> NodeFuture<LinkDest> {
    Box::pin(async move {
      let node = self;
      let link_dest = context
        .core
        .vfs
        .read_link(&node.0)
        .await
        .map_err(|e| throw(&format!("{}", e)))?;
      Ok(LinkDest(link_dest))
    })
    .compat()
    .to_boxed()
  }
}

impl From<ReadLink> for NodeKey {
  fn from(n: ReadLink) -> Self {
    NodeKey::ReadLink(n)
  }
}

///
/// A Node that represents reading a file and fingerprinting its contents.
///
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DigestFile(pub File);

impl WrappedNode for DigestFile {
  type Item = hashing::Digest;

  fn run(self, context: Context) -> NodeFuture<hashing::Digest> {
    Box::pin(async move {
      let content = context
        .core
        .vfs
        .read_file(&self.0)
        .map_err(|e| throw(&format!("{}", e)))
        .await?;
      context
        .core
        .store()
        .store_file_bytes(content.content, true)
        .map_err(|e| throw(&e))
        .await
    })
    .compat()
    .to_boxed()
  }
}

impl From<DigestFile> for NodeKey {
  fn from(n: DigestFile) -> Self {
    NodeKey::DigestFile(n)
  }
}

///
/// A Node that represents executing a directory listing that returns a Stat per directory
/// entry (generally in one syscall). No symlinks are expanded.
///
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Scandir(Dir);

impl WrappedNode for Scandir {
  type Item = Arc<DirectoryListing>;

  fn run(self, context: Context) -> NodeFuture<Arc<DirectoryListing>> {
    Box::pin(async move {
      let directory_listing = context
        .core
        .vfs
        .scandir(self.0)
        .await
        .map_err(|e| throw(&format!("{}", e)))?;
      Ok(Arc::new(directory_listing))
    })
    .compat()
    .to_boxed()
  }
}

impl From<Scandir> for NodeKey {
  fn from(n: Scandir) -> Self {
    NodeKey::Scandir(n)
  }
}

///
/// A Node that captures an store::Snapshot for a PathGlobs subject.
///
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Snapshot(pub Key);

impl Snapshot {
  fn create(context: Context, path_globs: PreparedPathGlobs) -> NodeFuture<store::Snapshot> {
    // Recursively expand PathGlobs into PathStats.
    // We rely on Context::expand tracking dependencies for scandirs,
    // and store::Snapshot::from_path_stats tracking dependencies for file digests.

    Box::pin(async move {
      let path_stats = context
        .expand(path_globs)
        .map_err(|e| throw(&format!("{}", e)))
        .await?;
      store::Snapshot::from_path_stats(context.core.store(), context.clone(), path_stats)
        .map_err(|e| throw(&format!("Snapshot failed: {}", e)))
        .await
    })
    .compat()
    .to_boxed()
  }

  pub fn lift_path_globs(item: &Value) -> Result<PreparedPathGlobs, String> {
    let globs = externs::project_multi_strs(item, "globs");

    let description_of_origin_field = externs::project_str(item, "description_of_origin");
    let description_of_origin = if description_of_origin_field.is_empty() {
      None
    } else {
      Some(description_of_origin_field)
    };

    let glob_match_error_behavior =
      externs::project_ignoring_type(item, "glob_match_error_behavior");
    let failure_behavior = externs::project_str(&glob_match_error_behavior, "value");
    let strict_glob_matching =
      StrictGlobMatching::create(failure_behavior.as_str(), description_of_origin)?;

    let conjunction_obj = externs::project_ignoring_type(item, "conjunction");
    let conjunction_string = externs::project_str(&conjunction_obj, "value");
    let conjunction = GlobExpansionConjunction::create(&conjunction_string)?;

    PathGlobs::new(globs.clone(), strict_glob_matching, conjunction)
      .parse()
      .map_err(|e| format!("Failed to parse PathGlobs for globs({:?}): {}", globs, e))
  }

  pub fn store_directory(core: &Arc<Core>, item: &hashing::Digest) -> Value {
    externs::unsafe_call(
      &core.types.construct_directory_digest,
      &[
        externs::store_utf8(&item.0.to_hex()),
        externs::store_i64(item.1 as i64),
      ],
    )
  }

  pub fn store_snapshot(core: &Arc<Core>, item: &store::Snapshot) -> Value {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for ps in &item.path_stats {
      match ps {
        &PathStat::File { ref path, .. } => {
          files.push(Self::store_path(path));
        }
        &PathStat::Dir { ref path, .. } => {
          dirs.push(Self::store_path(path));
        }
      }
    }
    externs::unsafe_call(
      &core.types.construct_snapshot,
      &[
        Self::store_directory(core, &item.digest),
        externs::store_tuple(&files),
        externs::store_tuple(&dirs),
      ],
    )
  }

  fn store_path(item: &Path) -> Value {
    externs::store_utf8_osstr(item.as_os_str())
  }

  fn store_file_content(context: &Context, item: &FileContent) -> Value {
    externs::unsafe_call(
      &context.core.types.construct_file_content,
      &[
        Self::store_path(&item.path),
        externs::store_bytes(&item.content),
        externs::store_bool(item.is_executable),
      ],
    )
  }

  pub fn store_files_content(context: &Context, item: &[FileContent]) -> Value {
    let entries: Vec<_> = item
      .iter()
      .map(|e| Self::store_file_content(context, e))
      .collect();
    externs::unsafe_call(
      &context.core.types.construct_files_content,
      &[externs::store_tuple(&entries)],
    )
  }
}

impl WrappedNode for Snapshot {
  type Item = Arc<store::Snapshot>;

  fn run(self, context: Context) -> NodeFuture<Arc<store::Snapshot>> {
    let lifted_path_globs = Self::lift_path_globs(&externs::val_for(&self.0));
    future::result(lifted_path_globs)
      .map_err(|e| throw(&format!("Failed to parse PathGlobs: {}", e)))
      .and_then(move |path_globs| Self::create(context, path_globs))
      .map(Arc::new)
      .to_boxed()
  }
}

impl From<Snapshot> for NodeKey {
  fn from(n: Snapshot) -> Self {
    NodeKey::Snapshot(n)
  }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DownloadedFile(pub Key);

impl DownloadedFile {
  fn load_or_download(
    &self,
    core: Arc<Core>,
    url: Url,
    digest: hashing::Digest,
  ) -> BoxFuture<store::Snapshot, String> {
    let file_name = try_future!(url
      .path_segments()
      .and_then(Iterator::last)
      .map(str::to_owned)
      .ok_or_else(|| format!("Error getting the file name from the parsed URL: {}", url)));

    Box::pin(async move {
      let maybe_bytes = core.store().load_file_bytes_with(digest, |_| ()).await?;
      if maybe_bytes.is_none() {
        DownloadedFile::download(core.clone(), url, file_name.clone(), digest)
          .compat()
          .await?;
      }
      core
        .store()
        .snapshot_of_one_file(PathBuf::from(file_name), digest, true)
        .await
    })
    .compat()
    .to_boxed()
  }

  fn download(
    core: Arc<Core>,
    url: Url,
    file_name: String,
    expected_digest: hashing::Digest,
  ) -> BoxFuture<(), String> {
    // TODO: Retry failures
    core
      .http_client
      .get(url.clone())
      .send()
      .compat()
      .map_err(|err| format!("Error downloading file: {}", err))
      .and_then(move |response| {
        // Handle common HTTP errors.
        if response.status().is_server_error() {
          Err(format!(
            "Server error ({}) downloading file {} from {}",
            response.status().as_str(),
            file_name,
            url,
          ))
        } else if response.status().is_client_error() {
          Err(format!(
            "Client error ({}) downloading file {} from {}",
            response.status().as_str(),
            file_name,
            url,
          ))
        } else {
          Ok(response)
        }
      })
      .and_then(move |response| {
        struct SizeLimiter<W: std::io::Write> {
          writer: W,
          written: usize,
          size_limit: usize,
        }

        impl<W: std::io::Write> Write for SizeLimiter<W> {
          fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
            let new_size = self.written + buf.len();
            if new_size > self.size_limit {
              Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Downloaded file was larger than expected digest",
              ))
            } else {
              self.written = new_size;
              self.writer.write_all(buf)?;
              Ok(buf.len())
            }
          }

          fn flush(&mut self) -> Result<(), std::io::Error> {
            self.writer.flush()
          }
        }

        let digest_and_bytes = async move {
          let mut hasher = hashing::WriterHasher::new(SizeLimiter {
            writer: bytes::BytesMut::with_capacity(expected_digest.1).writer(),
            written: 0,
            size_limit: expected_digest.1,
          });

          let mut response_stream = response.bytes_stream();
          while let Some(next_chunk) = response_stream.next().await {
            let chunk =
              next_chunk.map_err(|err| format!("Error reading URL fetch response: {}", err))?;
            hasher
              .write_all(&chunk)
              .map_err(|err| format!("Error hashing/capturing URL fetch response: {}", err))?;
          }
          let (digest, bytewriter) = hasher.finish();
          Ok((digest, bytewriter.writer.into_inner().freeze()))
        };
        digest_and_bytes.boxed().compat().to_boxed()
      })
      .and_then(move |(actual_digest, buf)| {
        if expected_digest != actual_digest {
          return future::err(format!(
            "Wrong digest for downloaded file: want {:?} got {:?}",
            expected_digest, actual_digest
          ))
          .to_boxed();
        }

        Box::pin(async move {
          let _ = core.store().store_file_bytes(buf, true).await?;
          Ok(())
        })
        .compat()
        .to_boxed()
      })
      .to_boxed()
  }
}

impl WrappedNode for DownloadedFile {
  type Item = Arc<store::Snapshot>;

  fn run(self, context: Context) -> NodeFuture<Arc<store::Snapshot>> {
    let value = externs::val_for(&self.0);
    let url_to_fetch = externs::project_str(&value, "url");

    let url = try_future!(Url::parse(&url_to_fetch)
      .map_err(|err| throw(&format!("Error parsing URL {}: {}", url_to_fetch, err))));

    let expected_digest = try_future!(lift_digest(&externs::project_ignoring_type(
      &value, "digest"
    ))
    .map_err(|str| throw(&str)));

    self
      .load_or_download(context.core, url, expected_digest)
      .map(Arc::new)
      .map_err(|err| throw(&err))
      .to_boxed()
  }
}

impl From<DownloadedFile> for NodeKey {
  fn from(n: DownloadedFile) -> Self {
    NodeKey::DownloadedFile(n)
  }
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct Task {
  params: Params,
  product: TypeId,
  task: tasks::Task,
  entry: Arc<rule_graph::Entry<Rule>>,
}

impl Task {
  fn gen_get(
    context: &Context,
    params: &Params,
    entry: &Arc<rule_graph::Entry<Rule>>,
    gets: Vec<externs::Get>,
  ) -> NodeFuture<Vec<Value>> {
    let get_futures = gets
      .into_iter()
      .map(|get| {
        let context = context.clone();
        let mut params = params.clone();
        let entry = entry.clone();
        let dependency_key = selectors::DependencyKey::JustGet(selectors::Get {
          product: get.product,
          subject: *get.subject.type_id(),
        });
        let entry = context
          .core
          .rule_graph
          .edges_for_inner(&entry)
          .ok_or_else(|| throw(&format!("no edges for task {:?} exist!", entry)))
          .and_then(|edges| {
            edges
              .entry_for(&dependency_key)
              .cloned()
              .ok_or_else(|| match get.declared_subject {
                Some(ty) if externs::is_union(ty) => {
                  let value = externs::get_value_from_type_id(ty);
                  match externs::call_method(
                    &value,
                    "non_member_error_message",
                    &[externs::val_for(&get.subject)],
                  ) {
                    Ok(err_msg) => throw(&externs::val_to_str(&err_msg)),
                    // If the non_member_error_message() call failed for any reason,
                    // fall back to a generic message.
                    Err(_e) => throw(&format!(
                      "Type {} is not a member of the {} @union",
                      get.subject.type_id(),
                      ty
                    )),
                  }
                }
                _ => throw(&format!(
                  "{:?} did not declare a dependency on {:?}",
                  entry, dependency_key
                )),
              })
          });
        // The subject of the get is a new parameter that replaces an existing param of the same
        // type.
        params.put(get.subject);
        future::result(entry)
          .and_then(move |entry| Select::new(params, get.product, entry).run(context.clone()))
      })
      .collect::<Vec<_>>();
    future::join_all(get_futures).to_boxed()
  }

  ///
  /// Given a python generator Value, loop to request the generator's dependencies until
  /// it completes with a result Value.
  ///
  fn generate(
    context: Context,
    params: Params,
    entry: Arc<rule_graph::Entry<Rule>>,
    generator: Value,
  ) -> NodeFuture<Value> {
    future::loop_fn(Value::from(externs::none()), move |input| {
      let context = context.clone();
      let params = params.clone();
      let entry = entry.clone();
      future::result(externs::generator_send(&generator, &input)).and_then(move |response| {
        match response {
          externs::GeneratorResponse::Get(get) => {
            Self::gen_get(&context, &params, &entry, vec![get])
              .map(|vs| future::Loop::Continue(vs.into_iter().next().unwrap()))
              .to_boxed()
          }
          externs::GeneratorResponse::GetMulti(gets) => {
            Self::gen_get(&context, &params, &entry, gets)
              .map(|vs| future::Loop::Continue(externs::store_tuple(&vs)))
              .to_boxed()
          }
          externs::GeneratorResponse::Break(val) => future::ok(future::Loop::Break(val)).to_boxed(),
        }
      })
    })
    .to_boxed()
  }
}

impl fmt::Debug for Task {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "Task({}, {}, {}, {})",
      self.task.func, self.params, self.product, self.task.cacheable,
    )
  }
}

impl WrappedNode for Task {
  type Item = Value;

  fn run(self, context: Context) -> NodeFuture<Value> {
    let params = self.params;
    let deps = {
      let edges = &context
        .core
        .rule_graph
        .edges_for_inner(&self.entry)
        .expect("edges for task exist.");
      future::join_all(
        self
          .task
          .clause
          .into_iter()
          .map(|type_id| {
            Select::new_from_edges(params.clone(), type_id, edges).run(context.clone())
          })
          .collect::<Vec<_>>(),
      )
    };

    let func = self.task.func;
    let entry = self.entry;
    let product = self.product;
    deps
      .then(move |deps_result| match deps_result {
        Ok(deps) => externs::call(&externs::val_for(&func.0), &deps),
        Err(failure) => Err(failure),
      })
      .then(move |task_result| match task_result {
        Ok(val) => match externs::get_type_for(&val) {
          t if t == context.core.types.coroutine => Self::generate(context, params, entry, val),
          t if t == product => ok(val),
          _ => err(throw(&format!(
            "{:?} returned a result value that did not satisfy its constraints: {:?}",
            func, val
          ))),
        },
        Err(failure) => err(failure),
      })
      .to_boxed()
  }
}

impl From<Task> for NodeKey {
  fn from(n: Task) -> Self {
    NodeKey::Task(Box::new(n))
  }
}

#[derive(Default)]
pub struct Visualizer {
  viz_colors: HashMap<String, String>,
}

impl NodeVisualizer<NodeKey> for Visualizer {
  fn color_scheme(&self) -> &str {
    "set312"
  }

  fn color(&mut self, entry: &Entry<NodeKey>, context: &<NodeKey as Node>::Context) -> String {
    let max_colors = 12;
    match entry.peek(context) {
      None => "white".to_string(),
      Some(Err(Failure::Throw(..))) => "4".to_string(),
      Some(Err(Failure::Invalidated)) => "12".to_string(),
      Some(Ok(_)) => {
        let viz_colors_len = self.viz_colors.len();
        self
          .viz_colors
          .entry(entry.node().product_str())
          .or_insert_with(|| format!("{}", viz_colors_len % max_colors + 1))
          .clone()
      }
    }
  }
}

pub struct Tracer;

impl NodeTracer<NodeKey> for Tracer {
  fn is_bottom(result: Option<Result<NodeResult, Failure>>) -> bool {
    match result {
      Some(Err(Failure::Invalidated)) => false,
      Some(Err(Failure::Throw(..))) => false,
      Some(Ok(_)) => true,
      None => {
        // A Node with no state is either still running, or effectively cancelled
        // because a dependent failed. In either case, it's not useful to render
        // them, as we don't know whether they would have succeeded or failed.
        true
      }
    }
  }

  fn state_str(indent: &str, result: Option<Result<NodeResult, Failure>>) -> String {
    match result {
      None => "<None>".to_string(),
      Some(Ok(ref x)) => format!("{:?}", x),
      Some(Err(Failure::Throw(ref x, ref traceback))) => format!(
        "Throw({})\n{}",
        externs::val_to_str(x),
        traceback
          .split('\n')
          .map(|l| format!("{}    {}", indent, l))
          .collect::<Vec<_>>()
          .join("\n")
      ),
      Some(Err(Failure::Invalidated)) => "Invalidated".to_string(),
    }
  }
}

///
/// There is large variance in the sizes of the members of this enum, so a few of them are boxed.
///
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum NodeKey {
  DigestFile(DigestFile),
  DownloadedFile(DownloadedFile),
  MultiPlatformExecuteProcess(Box<MultiPlatformExecuteProcess>),
  ReadLink(ReadLink),
  Scandir(Scandir),
  Select(Box<Select>),
  Snapshot(Snapshot),
  Task(Box<Task>),
}

impl NodeKey {
  fn product_str(&self) -> String {
    match self {
      &NodeKey::MultiPlatformExecuteProcess(..) => "ProcessResult".to_string(),
      &NodeKey::DownloadedFile(..) => "DownloadedFile".to_string(),
      &NodeKey::Select(ref s) => format!("{}", s.product),
      &NodeKey::Task(ref s) => format!("{}", s.product),
      &NodeKey::Snapshot(..) => "Snapshot".to_string(),
      &NodeKey::DigestFile(..) => "DigestFile".to_string(),
      &NodeKey::ReadLink(..) => "LinkDest".to_string(),
      &NodeKey::Scandir(..) => "DirectoryListing".to_string(),
    }
  }

  pub fn fs_subject(&self) -> Option<&Path> {
    match self {
      &NodeKey::DigestFile(ref s) => Some(s.0.path.as_path()),
      &NodeKey::ReadLink(ref s) => Some((s.0).0.as_path()),
      &NodeKey::Scandir(ref s) => Some((s.0).0.as_path()),

      // Not FS operations:
      // Explicitly listed so that if people add new NodeKeys they need to consider whether their
      // NodeKey represents an FS operation, and accordingly whether they need to add it to the
      // above list or the below list.
      &NodeKey::MultiPlatformExecuteProcess { .. }
      | &NodeKey::Select { .. }
      | &NodeKey::Snapshot { .. }
      | &NodeKey::Task { .. }
      | &NodeKey::DownloadedFile { .. } => None,
    }
  }

  pub fn display_info(&self) -> Option<&tasks::DisplayInfo> {
    match self {
      NodeKey::Task(ref task) => Some(&task.task.display_info),
      _ => None,
    }
  }
}

impl Node for NodeKey {
  type Context = Context;

  type Item = NodeResult;
  type Error = Failure;

  fn run(self, context: Context) -> NodeFuture<NodeResult> {
    let mut workunit_state = workunit_store::expect_workunit_state();

    let started_workunit_id = {
      let display = context.session.should_handle_workunits() && self.user_facing_name().is_some();
      let name = self.user_facing_name().unwrap_or(format!("{}", self));
      let span_id = new_span_id();
      let desc = self.display_info().and_then(|di| di.desc.as_ref().cloned());

      // We're starting a new workunit: record our parent, and set the current parent to our span.
      let parent_id = std::mem::replace(&mut workunit_state.parent_id, Some(span_id.clone()));
      let metadata = WorkunitMetadata {
        desc,
        display,
        blocked: false,
      };

      context
        .session
        .workunit_store()
        .start_workunit(span_id, name, parent_id, metadata)
    };

    scope_task_workunit_state(Some(workunit_state), async move {
      let context2 = context.clone();
      let maybe_watch = if let Some(path) = self.fs_subject() {
        let abs_path = context.core.build_root.join(path);
        context
          .core
          .watcher
          .watch(abs_path)
          .map_err(|e| Context::mk_error(&format!("{:?}", e)))
          .await
      } else {
        Ok(())
      };

      let result = match maybe_watch {
        Ok(()) => match self {
          NodeKey::DigestFile(n) => n.run(context).map(NodeResult::from).compat().await,
          NodeKey::DownloadedFile(n) => n.run(context).map(NodeResult::from).compat().await,
          NodeKey::MultiPlatformExecuteProcess(n) => {
            n.run(context).map(NodeResult::from).compat().await
          }
          NodeKey::ReadLink(n) => n.run(context).map(NodeResult::from).compat().await,
          NodeKey::Scandir(n) => n.run(context).map(NodeResult::from).compat().await,
          NodeKey::Select(n) => n.run(context).map(NodeResult::from).compat().await,
          NodeKey::Snapshot(n) => n.run(context).map(NodeResult::from).compat().await,
          NodeKey::Task(n) => n.run(context).map(NodeResult::from).compat().await,
        },
        Err(e) => Err(e),
      };
      context2
        .session
        .workunit_store()
        .complete_workunit(started_workunit_id)
        .unwrap();
      result
    })
    .boxed()
    .compat()
    .to_boxed()
  }

  fn digest(res: NodeResult) -> Option<hashing::Digest> {
    match res {
      NodeResult::Digest(d) => Some(d),
      NodeResult::DirectoryListing(_)
      | NodeResult::LinkDest(_)
      | NodeResult::ProcessResult(_)
      | NodeResult::Snapshot(_)
      | NodeResult::Value(_) => None,
    }
  }

  fn cacheable(&self) -> bool {
    match self {
      &NodeKey::Task(ref s) => s.task.cacheable,
      _ => true,
    }
  }

  fn user_facing_name(&self) -> Option<String> {
    match self {
      NodeKey::Task(ref task) => task.task.display_info.name.as_ref().map(|s| s.to_owned()),
      NodeKey::Snapshot(_) => Some(format!("{}", self)),
      NodeKey::MultiPlatformExecuteProcess(mp_epr) => mp_epr.0.user_facing_name(),
      NodeKey::DigestFile(..) => None,
      NodeKey::DownloadedFile(..) => None,
      NodeKey::ReadLink(..) => None,
      NodeKey::Scandir(..) => None,
      NodeKey::Select(..) => None,
    }
  }
}

impl Display for NodeKey {
  fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
    match self {
      &NodeKey::DigestFile(ref s) => write!(f, "DigestFile({:?})", s.0),
      &NodeKey::DownloadedFile(ref s) => write!(f, "DownloadedFile({:?})", s.0),
      &NodeKey::MultiPlatformExecuteProcess(ref s) => {
        write!(f, "MultiPlatformExecuteProcess({:?}", s.0)
      }
      &NodeKey::ReadLink(ref s) => write!(f, "ReadLink({:?})", s.0),
      &NodeKey::Scandir(ref s) => write!(f, "Scandir({:?})", s.0),
      &NodeKey::Select(ref s) => write!(f, "Select({}, {})", s.params, s.product,),
      &NodeKey::Task(ref s) => write!(f, "{:?}", s),
      &NodeKey::Snapshot(ref s) => write!(f, "Snapshot({})", format!("{}", &s.0)),
    }
  }
}

impl NodeError for Failure {
  fn invalidated() -> Failure {
    Failure::Invalidated
  }

  fn exhausted() -> Failure {
    Context::mk_error(
      "Exhausted retries for uncacheable node. The filesystem was changing too much.",
    )
  }

  fn cyclic(mut path: Vec<String>) -> Failure {
    let path_len = path.len();
    if path_len > 1 {
      path[0] += " <-";
      path[path_len - 1] += " <-"
    }
    throw(&format!(
      "Dep graph contained a cycle:\n  {}",
      path.join("\n  ")
    ))
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NodeResult {
  Digest(hashing::Digest),
  DirectoryListing(Arc<DirectoryListing>),
  LinkDest(LinkDest),
  ProcessResult(ProcessResult),
  Snapshot(Arc<store::Snapshot>),
  Value(Value),
}

impl From<Value> for NodeResult {
  fn from(v: Value) -> Self {
    NodeResult::Value(v)
  }
}

impl From<Arc<store::Snapshot>> for NodeResult {
  fn from(v: Arc<store::Snapshot>) -> Self {
    NodeResult::Snapshot(v)
  }
}

impl From<hashing::Digest> for NodeResult {
  fn from(v: hashing::Digest) -> Self {
    NodeResult::Digest(v)
  }
}

impl From<ProcessResult> for NodeResult {
  fn from(v: ProcessResult) -> Self {
    NodeResult::ProcessResult(v)
  }
}

impl From<LinkDest> for NodeResult {
  fn from(v: LinkDest) -> Self {
    NodeResult::LinkDest(v)
  }
}

impl From<Arc<DirectoryListing>> for NodeResult {
  fn from(v: Arc<DirectoryListing>) -> Self {
    NodeResult::DirectoryListing(v)
  }
}

impl TryFrom<NodeResult> for Value {
  type Error = ();

  fn try_from(nr: NodeResult) -> Result<Self, ()> {
    match nr {
      NodeResult::Value(v) => Ok(v),
      _ => Err(()),
    }
  }
}

impl TryFrom<NodeResult> for Arc<store::Snapshot> {
  type Error = ();

  fn try_from(nr: NodeResult) -> Result<Self, ()> {
    match nr {
      NodeResult::Snapshot(v) => Ok(v),
      _ => Err(()),
    }
  }
}

impl TryFrom<NodeResult> for hashing::Digest {
  type Error = ();

  fn try_from(nr: NodeResult) -> Result<Self, ()> {
    match nr {
      NodeResult::Digest(v) => Ok(v),
      _ => Err(()),
    }
  }
}

impl TryFrom<NodeResult> for ProcessResult {
  type Error = ();

  fn try_from(nr: NodeResult) -> Result<Self, ()> {
    match nr {
      NodeResult::ProcessResult(v) => Ok(v),
      _ => Err(()),
    }
  }
}

impl TryFrom<NodeResult> for LinkDest {
  type Error = ();

  fn try_from(nr: NodeResult) -> Result<Self, ()> {
    match nr {
      NodeResult::LinkDest(v) => Ok(v),
      _ => Err(()),
    }
  }
}

impl TryFrom<NodeResult> for Arc<DirectoryListing> {
  type Error = ();

  fn try_from(nr: NodeResult) -> Result<Self, ()> {
    match nr {
      NodeResult::DirectoryListing(v) => Ok(v),
      _ => Err(()),
    }
  }
}
