# Sleek Manifest File Handler

[nix]: https://nixos.org
[Hjem]: https://github.com/feel-co/hjem
[nixos-core]: https://github.com/feel-co/nixos-core

`smfh` is a declarative file manager. You write a JSON manifest describing the
files you want on disk, e.g., copies, symlinks, directories, permission changes,
and deletions and `smfh` brings the filesystem into that state. It is designed
for reliability; smfh boasts atomic replacements, BLAKE3 content verification,
and deterministic ordering.

The primary use case is managing dotfiles and system configuration from [Nix],
particularly with [Hjem]. It may also be used for `/etc` management in NixOS, as
demonstrated by the [nixos-core] project.

[CLI tool]: https://crates.io/crates/smfh
[Rust library]: https://crates.io/crates/smfh-core

smfh comes as a [CLI tool] and a [Rust library]. See above projects for a
demonstration of each use case.

## Manifest format

A manifest is a JSON object with a `version`, a list of `files`, and an optional
`clobber_by_default` flag.

```json
{
  "files": [
    {
      "type": "copy",
      "source": "./sources/file",
      "target": "./outputs/file",
      "permissions": null,
      "uid": null,
      "gid": null,
      "clobber": null,
      "ignore_modification": null
    },
    {
      "type": "symlink",
      "source": "./sources/file",
      "target": "./outputs/symlink",
      "permissions": null,
      "uid": null,
      "gid": null,
      "clobber": null,
      "follow_symlinks": null,
      "ignore_modification": null
    },
    {
      "type": "modify",
      "target": "./outputs/modified",
      "permissions": null,
      "uid": null,
      "gid": null
    },
    {
      "type": "directory",
      "target": "./outputs/directory",
      "permissions": null,
      "uid": null,
      "gid": null,
      "clobber": null
    },
    {
      "type": "delete",
      "target": "./outputs/delete"
    }
  ],
  "clobber_by_default": false,
  "version": 3
}
```

### Top-level fields

<!--markdownlint-disable MD013-->

| Field                | Type      | Required | Description                                                                                      |
| -------------------- | --------- | -------- | ------------------------------------------------------------------------------------------------ |
| `version`            | `number`  | yes      | Format version. Current maximum is `3`. Older versions are accepted for backwards compatibility. |
| `files`              | `File[]`  | yes      | The operations to perform.                                                                       |
| `clobber_by_default` | `boolean` | no       | If `true`, overwrite existing files instead of backing them up. Default `false`.                 |

<!--markdownlint-enable MD013-->

### File entry fields

<!--markdownlint-disable MD013-->

| Field                 | Type      | Required | Description                                                                                                                            |
| --------------------- | --------- | -------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| `type`                | `string`  | yes      | `copy`, `symlink`, `directory`, `modify`, or `delete`.                                                                                 |
| `target`              | `string`  | yes      | The path to create, modify, or remove.                                                                                                 |
| `source`              | `string`  | no       | Source path. Required for `copy` and `symlink`; must not be present for other types.                                                   |
| `permissions`         | `string`  | no       | Octal mode, e.g. `"755"`. Applied to `copy`, `symlink`, `directory`, and `modify`.                                                     |
| `uid`                 | `number`  | no       | Owner user ID. Applied to `copy`, `symlink`, `directory`, and `modify`.                                                                |
| `gid`                 | `number`  | no       | Owner group ID. Applied to `copy`, `symlink`, `directory`, and `modify`.                                                               |
| `clobber`             | `boolean` | no       | Override `clobber_by_default` for this entry.                                                                                          |
| `follow_symlinks`     | `boolean` | no       | Only for `symlink`. If `true` (default), the source is canonicalized to an absolute path. If `false`, the literal source path is used. |
| `ignore_modification` | `boolean` | no       | Only for `copy` and `symlink`. If `true`, skip content integrity checks during activation.                                             |
| `deactivate`          | `boolean` | no       | If `false`, this entry is skipped during `smfh deactivate`. Default `true`.                                                            |

<!--markdownlint-enable MD013-->

Any field set to `null` is treated as absent.

### File types

- **`copy`**: copies `source` to `target`. If the target exists and differs
  (checked via BLAKE3 hash and size), it is backed up with the configured prefix
  or clobbered. Parent directories are created automatically.
- **`symlink`**. creates a symbolic link at `target` pointing to `source`. If
  `follow_symlinks` is `true`, the source is resolved to an absolute path before
  linking.
- **`directory`**: creates `target` and any missing parent directories. If the
  target exists and is not a directory, it is backed up or clobbered first.
- **`modify`**: changes permissions and/or ownership of an existing file. Does
  not create the file if it is missing.
- **`delete`**: removes `target`. No `source` is used.

> [!NOTE]
> Files are applied in a fixed order regardless of their position in the JSON
> array:
>
> 1. Directories (shallowest paths first)
> 2. Copies
> 3. Symlinks
> 4. Modifies
> 5. Deletes (shallowest first, so `/a/b` is deleted before `/a`)
>
> This ensures parent directories exist before their contents, and deletions do
> not remove files that later entries might need.

### Atomic replacement

For `copy` and `symlink` entries, `smfh` attempts atomic replacement when the
target already exists and matches the expected state: the new content is written
to a random temporary file in the same directory, then renamed into place. If
the types are incompatible (e.g., replacing a directory with a file), it falls
back to backup-and-write.

### Example

With the `sources` directory containing:

```bash
$ eza --long --no-user --no-time --no-filesize --tree -L 2 sources
drwxr-xr-x sources
.rw-r--r-- └── file
```

And the `outputs` directory looking like this beforehand:

```bash
$ eza --long --no-user --no-time --no-filesize --tree -L 2 outputs
drwxr-xr-x outputs
.rw-r--r-- ├── delete
.rw-r--r-- └── modified
```

Running `smfh activate manifest.json` produces:

```bash
$ eza --long --no-user --no-time --no-filesize --tree -L 2 outputs
drwxr-xr-x outputs
drwxr-xr-x ├── directory
.rw-r--r-- ├── file
.rw-r--r-- ├── modified
lrwxrwxrwx └── symlink -> /absolute/path/sources/file
```

## CLI usage

```plaintext
smfh <COMMAND>

Commands:
  activate    Apply a manifest to the filesystem
  deactivate  Remove all files described by a manifest
  diff        Apply only the delta between an old and new manifest
  verify      Check a manifest for structural errors
  clean       Read, verify, and re-serialize a manifest (pretty-printed)
  help        Print this message or the help of the given subcommand(s)

Options:
  -v, --verbose  Enable info-level logging
      --impure   Allow relative paths and shell expansion in the manifest
  -h, --help     Print help
  -V, --version  Print version
```

### `activate`

Reads the manifest, verifies it, and applies every entry. Existing files that
differ from the manifest are backed up with the given prefix (unless clobbered).

### `deactivate`

Removes every file described by the manifest in reverse dependency order. Skips
entries with `deactivate: false`.

### `diff`

Compares two manifests and applies only the changes. Files removed from the new
manifest are deactivated; added or updated files are activated. If `--fallback`
is set and the old manifest is missing, a full activation is performed.

### `verify`

Checks the manifest for structural errors (missing sources, unexpected fields,
etc.). Exits with code 3 if errors are found.

### `clean`

Reads the manifest, verifies it, and prints a normalized JSON representation.
Useful for reformatting manifests or checking that they parse correctly.

## Exit codes

| Code | Meaning                                            |
| ---- | -------------------------------------------------- |
| 0    | Success                                            |
| 1    | Generic failure (activation or deactivation error) |
| 2    | Manifest/program version mismatch                  |
| 3    | Manifest deserialization or validation error       |
| 4    | Shell expansion failed (impure mode only)          |

## Hacking

### Building From Source

A Nix devshell is provided in `flake.nix`. Use `nix develop` to enter a
reproducible development environment and build with Cargo:

```bash
# Building in release mode
$ cargo build --release
```

The `smfh` binary is produced at `target/release/smfh`. Alternatively, you may
build and install from source with Cargo:

```bash
# Get smfh from crates.io
$ cargo install smfh --locked
```

This also works with a source installation using `cargo install, e.g.:

```bash
# Get smfh from feel-co/smfh
$ cargo install --git https://github.com/feel-co/smfh --locked
```

You'll need Rust 1.85.0 or above for Rust 2024 edition. Most distributions
should package this version already. You may, of course, prefer to package the
built releases if you'd like.

### Versioning

`smfh` follows semantic versioning for the library (`smfh-core`) and keeps the
CLI version in sync. The manifest format has its own version field, currently at
`3`. The tool accepts all manifest versions `=< 3` and rejects newer versions to
prevent misinterpretation.

## License

AGPL-3.0-only
