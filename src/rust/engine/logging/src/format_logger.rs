// Copyright 2020 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

#![deny(warnings)]
// Enable all clippy lints except for many of the pedantic ones. It's a shame this needs to be copied and pasted across crates, but there doesn't appear to be a way to include inner attributes from a common source.
#![deny(
clippy::all,
clippy::default_trait_access,
clippy::expl_impl_clone_on_copy,
clippy::if_not_else,
clippy::needless_continue,
clippy::unseparated_literal_suffix,
// TODO: Falsely triggers for async/await:
//   see https://github.com/rust-lang/rust-clippy/issues/5360
// clippy::used_underscore_binding
)]
// It is often more clear to show that nothing is being moved.
#![allow(clippy::match_ref_pats)]
// Subjective style.
#![allow(
clippy::len_without_is_empty,
clippy::redundant_field_names,
clippy::too_many_arguments
)]
// Default isn't as big a deal as people seem to think it is.
#![allow(clippy::new_without_default, clippy::new_ret_no_self)]
// Arc<Mutex> can be more clear than needing to grok Orderings:
#![allow(clippy::mutex_atomic)]

use std::fmt::Write;

use log::{Log, Metadata, Record};
use parking_lot::Mutex;
use simplelog::WriteLogger;

pub struct FormatLogger<W> {
  inner: Mutex<WriteLogger<W>>
}

impl<W: Write + Send + 'static> FormatLogger<W> {
  pub fn new(inner: WriteLogger<W>) -> FormatLogger<W> {
    FormatLogger {
      inner: Mutex::new(inner),
    }
  }
}

impl<W> Log for FormatLogger<W> {
  fn enabled(&self, metadata: &Metadata<'_>) -> bool {
    self.inner.lock().enabled(metadata)
  }

  fn log(&self, record: &Record<'_>) {
    if self.enabled(record.metadata()) {
      // Format the arguments into a string so that any calls into Python are eliminated
      // before we take any logging-related locks.
      let formatted_args = format!("{}", record.args());

      let record = Record::builder()
        .metadata(record.metadata().clone())
        .args(format_args!("{}", formatted_args))
        .module_path(record.module_path())
        .file(record.file())
        .line(record.line())
        .build();

      let mut l = self.inner.lock();
      l.log(&record);
    }
  }

  fn flush(&self) {
    let _ = self.inner.lock().flush();
  }
}
