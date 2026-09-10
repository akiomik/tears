# Contributing

Issues and pull requests are welcome.

Most of what a change here has to satisfy is written down already:

- [RFCs](docs/rfcs/README.md) — design contracts, invariants and decision
  records. A new RFC or an amendment runs the
  [pre-review checklist](docs/rfcs/pre-review-checklist.md) before review.
- [API Design Guidelines](docs/api-guidelines.md)
- [Testing Guidelines](docs/testing.md)
- [Releasing](docs/releasing.md)

## Language

Everything published is written in English, and that is wider than the tree:
commit messages, pull request and issue titles and bodies, and the comments on
them. Where something will be read decides it, not what kind of thing it is — a
review reply is as public as a page of documentation.

Leave out what a reader outside the repository cannot resolve. Issue and pull
request numbers, released version numbers and commit SHAs all travel; paths
`.gitignore` covers, local working-note filenames and work-in-progress labels do
not, and naming one leaves a reference nobody but its author can follow.

## Commit messages and pull request titles

Commit messages follow [Conventional Commits][cc]: `type(scope): summary`, with
`feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`,
`chore` and `bench` as the types. The scope names the area — usually a module
path (`fix(runtime)`), otherwise the part of the repository the change is in
(`docs(rfcs)`, `chore(deps)`); it is omitted where the change has no single one,
and holds a comma-separated pair where it genuinely has two
(`feat(command,subscription)`). A breaking change takes `!` before the `:`.

Pull request titles take the same shape.

This governs new commits; the log holds a few subjects with types not on this
list, from before it was written. Nothing verifies it either — there is no
commit linter here, and no CI job reads a commit message.

## The changelog

`CHANGELOG.md` follows [Keep a Changelog][kac]. Write the entry under
`## [Unreleased]` in the same pull request as the change. A release moves that
section under a version heading and edits the file in several other ways;
[docs/releasing.md](docs/releasing.md) has them.

An entry goes in when the change reaches someone who depends on the published
crate: its public API, its runtime behaviour, its MSRV, the dependency
requirements resolved alongside it, or a defect in the set of files it ships.
This repository's own work is not — documentation, examples, tests, benches, CI
and the crates.io listing — and neither is a lockfile-only bump, which moves no
requirement a dependant resolves against. Adding an example stays in that
second list even though `include` ships it: what earns an entry is the shipped
set being *wrong*, not its growing as intended.

Mark a breaking entry with a `**Breaking:**` prefix under its category heading
and put the before/after beside it. Pre-1.0 that also decides the version:
[docs/releasing.md](docs/releasing.md#choosing-the-version) has the rule, and
[docs/migrations/README.md](docs/migrations/README.md) says when a release owes
a migration guide as well.

All of this decides the next entry, not the ones above it. Released sections
record where the line fell when they were written — including the
`**BREAKING**:` marker used up to 0.9.0 — and they stay as they are.

## Documentation

An edit is not finished until the changed file's own documentation has been read
again — the module doc, every doc comment under it, the comments in the body —
and any disagreement with the code fixed on one side or the other, explicitly.

## Before you push

[just](https://github.com/casey/just) runs the local gate:

```console
$ just pre-commit
```

That is `fmt-check`, both clippy passes, the tests, the loom mirrors' rows and
the doctests. `just check` is the same list with `fmt` in place of `fmt-check`,
so it formats rather than reporting.

Neither recipe is the whole of CI, and two of the things they leave out are
worth running by hand:

- `Documentation` builds the docs with warnings denied. Nothing `check` or
  `pre-commit` reaches builds them at all, so a broken intra-doc link passes the
  local gate and blocks the merge. It renders twice, under the feature set the
  other jobs use and under the list `[package.metadata.docs.rs]` declares:

  ```console
  $ just doc-strict "$(just --evaluate build_features)"
  $ just doc-declared
  ```

- `Doc Tests` runs `just test-doc`, which `pre-commit` has, and then
  `just test-doc-packaged`, which it does not. That one packages the crate,
  extracts it and runs the doctests against what a consumer gets, which is where
  a missing `include` entry shows up and nowhere else:

  ```console
  $ just test-doc-packaged
  ```

Run them after touching rustdoc, renaming a public item, or moving `include` or
a feature list — and when in doubt, since neither failure shows up locally any
other way. Both read the manifest with `jq`, which `just pre-commit` does not
need.

[cc]: https://www.conventionalcommits.org/en/v1.0.0/
[kac]: https://keepachangelog.com/en/1.0.0/
