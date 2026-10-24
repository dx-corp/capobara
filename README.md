# Capobara

Capobara is the name of the binary, the crate, and the public repository it
publishes itself into. A capo transposes a whole tuning onto a new position
on the neck without changing the music, which is what a projection does to
a Mono tree.

`dx-corp/mono` is the source of truth. This repository is a projection of
`rust/tools/capobara`, published here by Capobara itself through the same
transport it implements. Mono's projection catalog registers the definition
at `config/projections/capobara.json`. The dedicated Code mirror also uses
Capobara for its `maestro-public-tree-v1` projection: package sanitization,
public documentation boundaries, vendored Rust dependencies, and stale-file
deletion run inside the binary. The Code workflow retains its release checks
and destination pull-request publication.

## Build

```
cargo build --locked
```

The crate is valid both as a Mono workspace member and standalone: it
declares its edition, license, Rust version, and dependency versions
directly rather than inheriting them, and this repository carries a
`Cargo.lock` committed at the crate root.

## Commands

| Command | Does |
| --- | --- |
| `capobara catalog` | Validate the catalog or print the publication matrix. |
| `capobara plan` | Report the plan without changing the destination. |
| `capobara apply` | Apply the projection to the destination checkout. |
| `capobara verify` | Apply and require the stored receipt to match. |
| `capobara check` | Report drift between source and destination. |
| `capobara prepare` | Clone-side preparation of the destination branch. |
| `capobara preflight` | Recheck a prepared projection before publication. |
| `capobara publish` | Commit, push, and open or update the pull request. |
| `capobara run` | Prepare, apply, verify, preflight, publish, and prove in one process. |

`plan`, `apply`, `verify`, and `check` support `copy-v1`,
`sdk-assembly-v1`, and the restricted `maestro-public-tree-v1` Code definition.
The Code definition stays outside the general catalog fanout; its dedicated
workflow owns version checks, prepared-tree guardrails, and auto-merge.

## Contributing

Issues are welcome here. Code changes land in `dx-corp/mono` and are
projected into this repository; pull requests opened directly against this
repository will be overwritten by the next projection.

## License

Business Source License 1.1 (BUSL-1.1). See [LICENSE](../../../LICENSE).
