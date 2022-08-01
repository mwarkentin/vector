use std::collections::{hash_map::Entry, HashMap};

use anymap::any::CloneAny;
use lookup::LookupBuf;
use value::{Kind, Value};

use crate::{parser::ast::Ident, type_def::Details, value::Collection};

/// Local environment, limited to a given scope.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LocalEnv {
    pub(crate) bindings: HashMap<Ident, Details>,
}

impl LocalEnv {
    pub(crate) fn variable_idents(&self) -> impl Iterator<Item = &Ident> + '_ {
        self.bindings.keys()
    }

    pub(crate) fn variable(&self, ident: &Ident) -> Option<&Details> {
        self.bindings.get(ident)
    }

    #[cfg(any(feature = "expr-assignment", feature = "expr-function_call"))]
    pub(crate) fn insert_variable(&mut self, ident: Ident, details: Details) {
        self.bindings.insert(ident, details);
    }

    #[cfg(feature = "expr-function_call")]
    pub(crate) fn remove_variable(&mut self, ident: &Ident) -> Option<Details> {
        self.bindings.remove(ident)
    }

    /// Any state the child scope modified that was part of the parent is copied to the parent scope
    pub(crate) fn apply_child_scope(mut self, child: Self) -> Self {
        for (ident, child_details) in child.bindings {
            if let Some(self_details) = self.bindings.get_mut(&ident) {
                *self_details = child_details;
            }
        }

        self
    }

    /// Merges two local envs together. This is useful in cases such as if statements
    /// where different `LocalEnv`'s can be created, and the result is decided at runtime.
    /// The compile-time type must be the union of the options.
    pub(crate) fn merge(mut self, other: Self) -> Self {
        for (ident, other_details) in other.bindings {
            if let Some(self_details) = self.bindings.get_mut(&ident) {
                *self_details = self_details.clone().merge(other_details);
            }
        }
        self
    }
}

/// A lexical scope within the program.
#[derive(Debug, Clone)]
pub struct ExternalEnv {
    /// The external target of the program.
    target: Details,

    read_only_paths: Vec<ReadOnlyPath>,

    /// Custom context injected by the external environment
    custom: anymap::Map<dyn CloneAny>,
}

// temporary until paths can point to metadata
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PathRoot {
    Event,
    Metadata,
}

#[derive(Debug, Clone)]
pub struct ReadOnlyPath {
    path: LookupBuf,
    recursive: bool,
    root: PathRoot,
}

impl Default for ExternalEnv {
    fn default() -> Self {
        Self::new_with_kind(Kind::object(Collection::any()))
    }
}

impl ExternalEnv {
    /// Creates a new external environment that starts with an initial given
    /// [`Kind`].
    #[must_use]
    pub fn new_with_kind(kind: Kind) -> Self {
        Self {
            target: Details {
                type_def: kind.into(),
                value: None,
            },
            custom: anymap::Map::new(),
            read_only_paths: vec![],
        }
    }

    pub fn is_read_only_event_path(&self, path: &LookupBuf) -> bool {
        self.is_read_only_path(path, PathRoot::Event)
    }

    pub fn is_read_only_metadata_path(&self, path: &LookupBuf) -> bool {
        self.is_read_only_path(path, PathRoot::Metadata)
    }

    pub(crate) fn is_read_only_path(&self, path: &LookupBuf, root: PathRoot) -> bool {
        for read_only_path in &self.read_only_paths {
            if read_only_path.root != root {
                continue;
            }

            // any paths that are a parent of read-only paths also can't be modified
            if read_only_path.path.starts_with(path) {
                return true;
            }

            if read_only_path.recursive {
                if path.starts_with(&read_only_path.path) {
                    return true;
                }
            } else if path == &read_only_path.path {
                return true;
            }
        }
        false
    }

    /// Adds a path that is considered read only. Assignments to any paths that match
    /// will fail at compile time.
    ///
    /// # Panics
    ///
    /// Panics if the path contains coalescing.
    pub(crate) fn add_read_only_path(&mut self, path: LookupBuf, recursive: bool, root: PathRoot) {
        assert!(
            !path
                .as_segments()
                .iter()
                .any(lookup::SegmentBuf::is_coalesce),
            "Coalesced paths not supported for read-only paths"
        );
        self.read_only_paths.push(ReadOnlyPath {
            path,
            recursive,
            root,
        });
    }

    pub fn add_read_only_event_path(&mut self, path: LookupBuf, recursive: bool) {
        self.add_read_only_path(path, recursive, PathRoot::Event);
    }

    pub fn add_read_only_metadata_path(&mut self, path: LookupBuf, recursive: bool) {
        self.add_read_only_path(path, recursive, PathRoot::Metadata);
    }

    pub(crate) fn target(&self) -> &Details {
        &self.target
    }

    pub(crate) fn target_mut(&mut self) -> &mut Details {
        &mut self.target
    }

    pub fn target_kind(&self) -> &Kind {
        self.target().type_def.kind()
    }

    #[cfg(any(feature = "expr-assignment", feature = "expr-query"))]
    pub(crate) fn update_target(&mut self, details: Details) {
        self.target = details;
    }

    /// Sets the external context data for VRL functions to use.
    pub fn set_external_context<T: 'static + CloneAny>(&mut self, data: T) {
        self.custom.insert::<T>(data);
    }

    /// Marks everything as read only. Any mutations on read-only values will result in a
    /// compile time error.
    pub fn read_only(mut self) -> Self {
        self.add_read_only_event_path(LookupBuf::root(), true);
        self.add_read_only_metadata_path(LookupBuf::root(), true);
        self
    }

    /// Get external context data from the external environment.
    pub fn get_external_context<T: 'static + CloneAny>(&self) -> Option<&T> {
        self.custom.get::<T>()
    }

    /// Swap the existing external contexts with new ones, returning the old ones.
    #[must_use]
    #[cfg(feature = "expr-function_call")]
    pub(crate) fn swap_external_context(
        &mut self,
        ctx: anymap::Map<dyn CloneAny>,
    ) -> anymap::Map<dyn CloneAny> {
        std::mem::replace(&mut self.custom, ctx)
    }
}

/// The state used at runtime to track changes as they happen.
#[derive(Debug, Default)]
pub struct Runtime {
    /// The [`Value`] stored in each variable.
    variables: HashMap<Ident, Value>,
}

impl Runtime {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.variables.is_empty()
    }

    pub fn clear(&mut self) {
        self.variables.clear();
    }

    #[must_use]
    pub fn variable(&self, ident: &Ident) -> Option<&Value> {
        self.variables.get(ident)
    }

    pub fn variable_mut(&mut self, ident: &Ident) -> Option<&mut Value> {
        self.variables.get_mut(ident)
    }

    pub(crate) fn insert_variable(&mut self, ident: Ident, value: Value) {
        self.variables.insert(ident, value);
    }

    pub(crate) fn remove_variable(&mut self, ident: &Ident) {
        self.variables.remove(ident);
    }

    pub(crate) fn swap_variable(&mut self, ident: Ident, value: Value) -> Option<Value> {
        match self.variables.entry(ident) {
            Entry::Occupied(mut v) => Some(std::mem::replace(v.get_mut(), value)),
            Entry::Vacant(v) => {
                v.insert(value);
                None
            }
        }
    }
}
