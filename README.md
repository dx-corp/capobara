# Capobara

Capobara is the name of the binary, the crate, and the public repository it
publishes itself into. A capo transposes a whole tuning onto a new position
on the neck without changing the music, which is what a projection does to
a Mono tree.

`dx-corp/mono` is the source of truth. This repository is a projection of
`rust/tools/capobara`, published here by Capobara itself through the same
transport it implements — the same transport that will publish the rest
of Mono's projection catalog after cutover. The definition is registered
in Mono's projection catalog (`config/projections/capobara.json`); wiring
it into the automated publish matrix lands in a follow-up change.

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

`catalog`, `prepare`, `preflight`, `publish`, and `run` are landing
incrementally; track progress in `dx-corp/mono`.

## Contributing

Issues are welcome here. Code changes land in `dx-corp/mono` and are
projected into this repository; pull requests opened directly against this
repository will be overwritten by the next projection.

## License

Business Source License 1.1 (BUSL-1.1). See [LICENSE](LICENSE).
