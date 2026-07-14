//! xmlcarve — stream giant XML files into JSONL by record element with
//! constant memory.
//!
//! The crate is split into small, pure modules so every stage of the
//! pipeline is unit-testable without touching the filesystem:
//!
//! - [`entity`] — XML entity and character-reference decoding.
//! - [`xml`] — a streaming pull parser over any `Read`, constant memory.
//! - [`selector`] — record-element selector parsing and path matching.
//! - [`json`] — a minimal ordered JSON value tree and serializer.
//! - [`record`] — maps one XML record subtree onto a JSON value.
//! - [`carve`] — the streaming driver: events in, JSONL lines out.
//! - [`inspect`] — structure profiler that suggests a record selector.
//! - [`cli`] — argument parsing and command dispatch.

pub mod carve;
pub mod cli;
pub mod entity;
pub mod inspect;
pub mod json;
pub mod record;
pub mod selector;
pub mod xml;
