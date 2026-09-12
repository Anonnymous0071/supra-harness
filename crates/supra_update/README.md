# supra_update

Local signed update verification and Unix application. **T30** of the stage sequence.

## Trust chain

`check_local` reads only operator-supplied local files. It verifies a Minisign
signature over canonical manifest bytes, parses a strict schema with unknown
fields denied, then binds the requested target and exact archive filename,
version, compressed size and SHA-256, and the sole executable member's path,
size, and SHA-256. There is no network or live-release probe.

The archive is bounded to 128 MiB and 64 headers. It must contain exactly one
signed regular executable member; traversal, platform prefixes, links, special
files, duplicates, unsigned extras, malformed paths, and expansion beyond the
signed size are refused.

`apply_local` re-runs the archive verification while writing. On Unix it takes a
per-destination advisory lock, rejects symlink and non-file destinations,
records the destination identity, creates a same-directory staging file with
`create_new`, writes mode `0755`, syncs the file, rechecks type and identity,
atomically renames, and syncs the parent directory. Windows supports checking
but application fails closed until equivalent replacement semantics are
implemented.

Release packaging emits `supra-<target>.tar.gz` with only
`supra-<target>/supra` and a canonical `supra-<target>.manifest.json`.
`sign-release.sh` signs manifests only and refuses absent signing material.
