// Copyright 2021 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

use std::env;

use super::id::{NameTransform, OptionId};
use super::scope::{is_valid_scope_name, Scope};
use super::{DictEdit, OptionsSource};
use crate::cli_alias::{expand_aliases, AliasMap};
use crate::fromfile::FromfileExpander;
use crate::parse::{ParseError, Parseable};
use crate::ListEdit;
use core::iter::once;
use itertools::{chain, Itertools};
use parking_lot::Mutex;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Arg {
    context: Scope,
    flag: String,
    value: Option<String>,
}

impl Arg {
    /// Checks if this arg's flag is equal to the provided strings concatenated with dashes.
    /// E.g., "--foo-bar" matches ["-", "foo", "bar"].
    fn _flag_match<'a>(&self, dash_separated_strs: impl Iterator<Item = &'a str>) -> bool {
        #[allow(unstable_name_collisions)]
        // intersperse is provided by itertools::Itertools, but is also in the Rust nightly
        // as an experimental feature of standard Iterator. If/when that becomes standard we
        // can use it, but for now we must squelch the name collision.
        itertools::equal(
            self.flag.chars(),
            dash_separated_strs
                .map(str::chars)
                .intersperse("-".chars())
                .flatten(),
        )
    }

    fn _prefix<'a>(negate: bool) -> impl Iterator<Item = &'a str> {
        if negate {
            once("--no")
        } else {
            once("-")
        }
    }

    // Check if --scope-flag matches.
    fn _matches_explicit_scope(&self, id: &OptionId, negate: bool) -> bool {
        self._flag_match(chain![
            Self::_prefix(negate),
            once(id.scope.name()),
            id.name_components_strs()
        ])
    }

    // Check if --flag matches in the context of the current goal's scope.
    fn _matches_implicit_scope(&self, id: &OptionId, negate: bool) -> bool {
        self.context == id.scope
            && self._flag_match(chain![Self::_prefix(negate), id.name_components_strs()])
    }

    // Check if -s matches for a short name s, if any.
    fn _matches_short(&self, id: &OptionId) -> bool {
        if let Some(sn) = &id.short_name {
            self._flag_match(chain![once(""), once(sn.as_ref())])
        } else {
            false
        }
    }

    /// Checks if this arg provides a value for the specified option, either negated or not.
    fn _matches(&self, id: &OptionId, negate: bool) -> bool {
        self._matches_explicit_scope(id, negate)
            || self._matches_implicit_scope(id, negate)
            || self._matches_short(id)
    }

    fn matches(&self, id: &OptionId) -> bool {
        self._matches(id, false)
    }

    fn matches_negation(&self, id: &OptionId) -> bool {
        self._matches(id, true)
    }
}

#[derive(Debug)]
pub struct Args {
    // The arg strings this struct was instantiated with.
    arg_strs: Vec<String>,

    // The structured args parsed from the arg strings.
    args: Vec<Arg>,
    passthrough_args: Option<Vec<String>>,
}

impl Args {
    // Create an Args instance with the provided args, which must *not* include the
    // argv[0] process name.
    pub fn new<I: IntoIterator<Item = String>>(arg_strs: I) -> Self {
        let arg_strs = arg_strs.into_iter().collect::<Vec<_>>();
        let mut args: Vec<Arg> = vec![];
        let mut passthrough_args: Option<Vec<String>> = None;
        let mut scope = Scope::Global;

        let mut args_iter = arg_strs.iter();
        while let Some(arg_str) = args_iter.next() {
            if arg_str == "--" {
                // We've hit the passthrough args delimiter (`--`).
                passthrough_args = Some(args_iter.cloned().collect::<Vec<String>>());
                break;
            } else if arg_str.starts_with("--") {
                let mut components = arg_str.splitn(2, '=');
                let flag = components.next().unwrap();
                args.push(Arg {
                    context: scope.clone(),
                    flag: flag.to_string(),
                    value: components.next().map(str::to_string),
                });
            } else if arg_str.starts_with('-') && arg_str.len() >= 2 {
                let (flag, mut value) = arg_str.split_at(2);
                // We support -ldebug and -l=debug, so strip that extraneous equals sign.
                if let Some(stripped) = value.strip_prefix('=') {
                    value = stripped;
                }
                args.push(Arg {
                    context: scope.clone(),
                    flag: flag.to_string(),
                    value: if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    },
                });
            } else if is_valid_scope_name(arg_str) {
                scope = Scope::Scope(arg_str.to_string())
            } else {
                // The arg is a spec, so revert to global context for any trailing flags.
                scope = Scope::Global;
            }
        }

        Self {
            arg_strs,
            args,
            passthrough_args,
        }
    }

    pub fn argv() -> Self {
        let mut args = env::args().collect::<Vec<_>>().into_iter();
        args.next(); // Consume the process name (argv[0]).
        Self::new(env::args().collect::<Vec<_>>())
    }

    pub fn expand_aliases(&self, alias_map: &AliasMap) -> Self {
        Self::new(expand_aliases(self.arg_strs.clone(), alias_map))
    }
}

pub(crate) struct ArgsTracker {
    unconsumed_args: Mutex<HashSet<Arg>>,
}

impl ArgsTracker {
    fn new(args: &Args) -> Self {
        Self {
            unconsumed_args: Mutex::new(args.args.clone().into_iter().collect()),
        }
    }

    fn consume_arg(&self, arg: &Arg) {
        self.unconsumed_args.lock().remove(arg);
    }

    pub fn get_unconsumed_flags(&self) -> HashMap<Scope, Vec<String>> {
        // Map from positional context (GLOBAL or a goal name) to unconsumed flags encountered
        // at that position in the CLI args.
        let mut ret: HashMap<Scope, Vec<String>> = HashMap::new();
        for arg in self.unconsumed_args.lock().iter() {
            if let Some(flags_for_context) = ret.get_mut(&arg.context) {
                flags_for_context.push(arg.flag.clone());
            } else {
                let flags_for_context = vec![arg.flag.clone()];
                ret.insert(arg.context.clone(), flags_for_context);
            };
        }
        for entry in ret.iter_mut() {
            entry.1.sort(); // For stability in tests and when reporting unconsumed args.
        }
        ret
    }
}

pub(crate) struct ArgsReader {
    args: Args,
    fromfile_expander: FromfileExpander,
    tracker: Arc<ArgsTracker>,
}

impl ArgsReader {
    pub fn new(args: Args, fromfile_expander: FromfileExpander) -> Self {
        let tracker = Arc::new(ArgsTracker::new(&args));
        Self {
            args,
            fromfile_expander,
            tracker,
        }
    }

    pub fn expand_aliases(&self, alias_map: &AliasMap) -> Self {
        Self::new(
            self.args.expand_aliases(alias_map),
            self.fromfile_expander.clone(),
        )
    }

    pub fn get_args(&self) -> Vec<String> {
        self.args.arg_strs.clone()
    }

    pub fn get_passthrough_args(&self) -> Option<Vec<String>> {
        self.args.passthrough_args.clone()
    }

    pub fn get_tracker(&self) -> Arc<ArgsTracker> {
        self.tracker.clone()
    }

    fn matches(&self, arg: &Arg, id: &OptionId) -> bool {
        let ret = arg.matches(id);
        if ret {
            self.tracker.consume_arg(arg);
        }
        ret
    }

    fn matches_negation(&self, arg: &Arg, id: &OptionId) -> bool {
        let ret = arg.matches_negation(id);
        if ret {
            self.tracker.consume_arg(arg);
        }
        ret
    }

    fn to_bool(&self, arg: &Arg) -> Result<Option<bool>, ParseError> {
        // An arg can represent a bool either by having an explicit value parseable as a bool,
        // or by having no value (in which case it represents true).
        match &arg.value {
            Some(value) => match self.fromfile_expander.expand(value.to_string())? {
                Some(s) => bool::parse(&s).map(Some),
                _ => Ok(None),
            },
            None => Ok(Some(true)),
        }
    }

    fn get_list<T: Parseable>(&self, id: &OptionId) -> Result<Option<Vec<ListEdit<T>>>, String> {
        let mut edits = vec![];
        for arg in &self.args.args {
            if self.matches(arg, id) {
                let value = arg.value.as_ref().ok_or_else(|| {
                    format!("Expected list option {} to have a value.", self.display(id))
                })?;
                if let Some(es) = self
                    .fromfile_expander
                    .expand_to_list::<T>(value.to_string())
                    .map_err(|e| e.render(&arg.flag))?
                {
                    edits.extend(es);
                }
            }
        }
        if edits.is_empty() {
            Ok(None)
        } else {
            Ok(Some(edits))
        }
    }
}

impl OptionsSource for ArgsReader {
    fn display(&self, id: &OptionId) -> String {
        format!(
            "--{}{}",
            match &id.scope {
                Scope::Global => "".to_string(),
                Scope::Scope(scope) => format!("{}-", scope.to_ascii_lowercase()),
            },
            id.name("-", NameTransform::ToLower)
        )
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_string(&self, id: &OptionId) -> Result<Option<String>, String> {
        // We iterate in reverse so that the rightmost arg wins in case an option
        // is specified multiple times.
        for arg in self.args.args.iter().rev() {
            if self.matches(arg, id) {
                return self
                    .fromfile_expander
                    .expand(arg.value.clone().ok_or_else(|| {
                        format!("Expected list option {} to have a value.", self.display(id))
                    })?)
                    .map_err(|e| e.render(&arg.flag));
            };
        }
        Ok(None)
    }

    fn get_bool(&self, id: &OptionId) -> Result<Option<bool>, String> {
        // We iterate in reverse so that the rightmost arg wins in case an option
        // is specified multiple times.
        for arg in self.args.args.iter().rev() {
            if self.matches(arg, id) {
                return self.to_bool(arg).map_err(|e| e.render(&arg.flag));
            } else if self.matches_negation(arg, id) {
                return self
                    .to_bool(arg)
                    .map(|ob| ob.map(|b| b ^ true))
                    .map_err(|e| e.render(&arg.flag));
            }
        }
        Ok(None)
    }

    fn get_bool_list(&self, id: &OptionId) -> Result<Option<Vec<ListEdit<bool>>>, String> {
        self.get_list::<bool>(id)
    }

    fn get_int_list(&self, id: &OptionId) -> Result<Option<Vec<ListEdit<i64>>>, String> {
        self.get_list::<i64>(id)
    }

    fn get_float_list(&self, id: &OptionId) -> Result<Option<Vec<ListEdit<f64>>>, String> {
        self.get_list::<f64>(id)
    }

    fn get_string_list(&self, id: &OptionId) -> Result<Option<Vec<ListEdit<String>>>, String> {
        self.get_list::<String>(id)
    }

    fn get_dict(&self, id: &OptionId) -> Result<Option<Vec<DictEdit>>, String> {
        let mut edits = vec![];
        for arg in self.args.args.iter() {
            if self.matches(arg, id) {
                let value = arg.value.clone().ok_or_else(|| {
                    format!("Expected dict option {} to have a value.", self.display(id))
                })?;
                if let Some(es) = self
                    .fromfile_expander
                    .expand_to_dict(value)
                    .map_err(|e| e.render(&arg.flag))?
                {
                    edits.extend(es);
                }
            }
        }
        if edits.is_empty() {
            Ok(None)
        } else {
            Ok(Some(edits))
        }
    }
}
