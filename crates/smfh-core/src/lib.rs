#![expect(
    clippy::too_long_first_doc_paragraph,
    reason = "I'm too yappy for my own good" // P.S. it's the masked links
)]
//! Core library for the [Sleek Manifest File Handler](https://github.com/feel-co/smfh).
//!
//! # Overview
//!
//! `smfh-core` defines the data model and filesystem operations for managing
//! declarative file manifests, i.e., JSON documents that describe a desired set
//! of symlinks, copies, directories, permission modifications, and deletions.
//!
//! A [`Manifest`](crate::manifest::Manifest) is a list of
//! [`File`](crate::manifest::File) entries, each specifying a `target` path, a
//! `type` ([`FileKind`](crate::manifest::FileKind)), and optional properties
//! (source, permissions, ownership, clobber behavior, etc.).
//!
//! # Entry ordering
//!
//! Files are processed in a deterministic order: directories first, then
//! copies, symlinks, modifies, and finally deletes. Within the same kind,
//! entries are ordered by path depth (shallowest first). This ensures parent
//! directories exist before their contents and deletions happen last.
//!
//! # Versioning
//!
//! Each manifest carries a `version` field checked against
//! [`VERSION`] at read time. A manifest with a version
//! exceeding the libraryʼs [`VERSION`] is rejected with
//! [`ReadError::VersionTooNew`](crate::manifest::ReadError::VersionTooNew).

/// Filesystem utilities: directory creation, backup moves, file hashing, and
/// the [`FileWithMetadata`](crate::file_util::FileWithMetadata) type that
/// performs the actual activation and deactivation operations.
pub mod file_util;

/// Manifest data model: [`Manifest`](crate::manifest::Manifest),
/// [`File`](crate::manifest::File), [`FileKind`](crate::manifest::FileKind),
/// and the error types returned by reading, verifying, and diffing manifests.
pub mod manifest;

/// The current manifest format version supported by this library. Manifests
/// with a `version` field higher than this are rejected at read
/// time with [`ReadError::VersionTooNew`](crate::manifest::ReadError::VersionTooNew).
pub const VERSION: u64 = 3;
