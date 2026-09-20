# Contributing

This repository is a projection of `dx-corp/mono`, the source of truth for
Capobara. The tree here is regenerated from Mono's `rust/tools/capobara`
directory by Capobara itself, and every generated update arrives as a pull
request on the `sync/mono-projection` branch with a
`.repository-projection.json` receipt that names the exact Mono revision.

- Issues are welcome here.
- Code changes land in Mono. Pull requests opened against this repository
  are closed with a pointer to the Mono change they should become.
- The files this repository owns, and that projections never touch, are
  `.github/**`, `SECURITY.md`, and this file.
