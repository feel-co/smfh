# smfh-core

Core library for the Sleek Manifest File Handler. Defines the manifest data
model and the filesystem operations that turn a JSON manifest into a live
directory tree. This crate is designed for use from Rust and from the
[`smfh`](../smfh-cli/) CLI. It handles everything from JSON parsing to atomic
file replacement.

## Library API

The main types are:

- [`Manifest`](src/manifest.rs) is the deserialized manifest with methods to
  read, verify, activate, deactivate, and diff.
- [`File`](src/manifest.rs) is a single entry in the manifest.
- [`FileKind`](src/manifest.rs) is the operation type (`Copy`, `Symlink`,
  `Directory`, `Modify`, `Delete`).
- [`FileWithMetadata`](src/file_util.rs) is a `File` paired with live filesystem
  metadata. Performs the actual activation, deactivation, and integrity checks.

[docs.rs]: https://docs.rs/smfh/1.5.0/smfh

Module documentation can also be found on [docs.rs]. Contributions regarding
library documentation and testing are welcome.

### Lifecycle

```rust
use smfh_core::manifest::Manifest;
use std::path::Path;

// 1. Read and parse
let mut manifest = Manifest::read(Path::new("manifest.json"), false)?;

// 2. Validate structure
let errors = manifest.verify();
assert!(errors.is_empty());

// 3. Apply to filesystem
let failures = manifest.activate(".backup-");
```

### Diff updates

`Manifest::diff` compares two manifests and applies only the delta. Files
removed from the new manifest are deactivated; added or changed files are
activated. For `copy` and `symlink` entries whose targets already exist and
match the expected state, an atomic rename is attempted: the new content is
written to a random temporary file in the same directory, then renamed into
place.

```rust
manifest.diff(Path::new("old.json"), ".backup-", true)?;
```

If `fallback` is `true` and the old manifest does not exist, the call falls back
to a full activation.

### Entry ordering

Files are applied in a deterministic order:

1. Directories (shallowest first)
2. Copies
3. Symlinks
4. Modifies
5. Deletes

This ensures parent directories exist before their contents, and deletions
happen last.

### Integrity checking

During activation and deactivation, `FileWithMetadata::check` verifies that the
live file matches the manifest:

- For `copy`: BLAKE3 hash and file size must match.
- For `symlink`: the resolved path must match the source.
- For all types: permissions and ownership must match if specified.

If `ignore_modification` is set, content checks are skipped.

### Error types

- [`ReadError`](src/manifest.rs) - manifest parsing or version mismatch.
- [`VerifyError`](src/manifest.rs) - structural validation failure (missing
  source, unexpected fields, etc.).
- [`DiffError`](src/manifest.rs) - diff application failure.

[`color-eyre`]: https://docs.rs/color-eyre

All filesystem errors are wrapped with [`color-eyre`] for context.

### Impure mode

In pure mode (default), only absolute paths without `..` components are
accepted; relative paths are silently discarded. In impure mode, paths are
shell-expanded via `shellexpand` and relative paths are allowed.

## License

AGPL-3.0-only
