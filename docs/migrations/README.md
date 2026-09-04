# Migration guides

Per-release upgrade guides for breaking changes that the compiler does not
fully report.

## When a release gets a guide

A guide is warranted when a release's breaking changes include **behavior a
caller must reason about but the compiler cannot name** — an ordering
guarantee that no longer holds, a property that is gone and not restorable, a
telemetry field whose meaning changed under a stable schema.

A release whose breaking changes are entirely type-level does **not** get a
guide. A renamed or removed item is already reported at every call site, and
the [changelog](../../CHANGELOG.md) entry carries the before/after next to the
change it belongs to. Adding a guide for those would split one story across
two files and put the maintenance burden on the wrong one.

The changelog is organised by *what changed*. A guide is organised by *what
the reader must do*, and exists to answer a question the changelog cannot:
which of these changes reaches me, and how do I tell?

## Conventions

- A guide is included into the crate under `#[cfg(doctest)]`, so its "after"
  snippets are compiled by `cargo test --doc` and cannot drift from the API.
  It adds no public module, so it can be deleted once the release it covers is
  far enough back.
- **Adding a guide is four edits besides the file itself, and only one of
  them fails loudly.** Removing a guide undoes the first three; the fourth
  is repointed rather than removed.

  - `include_str!` in `src/lib.rs`, under `cfg(doctest)`.
  - The file in `include` in `Cargo.toml`, named individually so the
    maintainer-facing files here (this README) stay out of the package.
    **This is the loud one**, and only just: nothing fails until someone
    runs `cargo test --doc` against the *published* crate, because `cargo
    publish` verifies with `cargo build`, which drops the `cfg(doctest)`
    item before the macro runs. `just test-doc-packaged` reproduces the
    published tree and is what the CI `doctest` job runs.
  - A row in the index below. Miss it and nothing fails at all — the guide
    is merely unfindable from here.
  - A pointer from the release's own `CHANGELOG.md` section, which is where
    an upgrading reader starts and the only one of these paths they are
    likely to be on. 0.11.0's is the blockquote above its `Added` list, and
    it is repo-relative. Nothing reports this one either. When the guide is
    eventually deleted, repoint this link at the release's tag rather than
    dropping it: the entry describes a past release and stays as it was
    written, and that release's published notes already link the guide
    there.
- "Before" snippets are marked `ignore`; they describe an API that is gone.
  Where "this no longer builds" is the point being taught, use `compile_fail`
  against the current API instead.
- The changelog keeps its per-entry before/after blocks. A guide links to the
  changelog rather than restating it, and holds only the triage, the detection
  steps, and the properties that are not restorable.
- **Links out of a guide must be absolute.** A guide is `include_str!`-ed into
  rustdoc, where it is rendered from `src/`, so a repo-relative path like
  `../../CHANGELOG.md` resolves in neither docs.rs nor a local `cargo doc`.
  Use the full `https://github.com/akiomik/tears/...` URL. (Anchors within the
  guide are fine, and are checked by nothing — verify them when editing
  headings.)

## Index

| Guide | Covers |
| --- | --- |
| [0.10.x → 0.11.0](0.10-to-0.11.md) | Reducer-first core: frame rate removal, one-lane delivery, `EffectCommand`, quit ordering |
