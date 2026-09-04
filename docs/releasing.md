# Releasing

A release is four steps that get progressively harder to take back. This
file is what "cut a release" means;
[`release.yml`](../.github/workflows/release.yml) automates only the last of
them.

It exists because the alternative is reading the previous release's diff,
and that decays. Through 0.8 each minor bump moved every dependency snippet
there was. 0.9.0 moved five of the seven; 0.10.0 and 0.11.0 moved only the
install snippet. No step was ever visibly skipped — each release's diff was
a plausible model for the next one, and each copy carried less than the one
before.

## The four steps

| | Produces | Taking it back |
| --- | --- | --- |
| 1 | the release PR | edit it |
| 2 | the merge commit | another commit |
| 3 | the tag and the GitHub release | cancel step 4, delete both, re-cut |
| 4 | the crates.io upload | nothing; `cargo yank` only stops resolutions |

Most of what follows is one of three consequences.

- **Whatever can be checked, check in step 1.** Not because the later steps
  cannot fail, but because their failures cost more.
- **Step 3 is one action, not two.** Publishing the GitHub release is what
  creates and pushes the tag; a draft release creates neither, and the push
  is what starts step 4.
- **Step 3's undo begins by stopping step 4.** Publishing already started the
  publish job, and it is running against the tree you are discarding. Cancel
  that run in the Actions tab first: deleting the release and the tag does not
  stop it, and the minutes spent deleting them are minutes it is still
  running. Wait for that run to show a terminal state — cancelling asks, it
  does not stop — and only then find out whether it got there first: if the
  upload landed, the number is spent, the row above no longer applies, and
  "When step 4 fails" does. Otherwise remove the two, separately — the
  release, then the tag (`git push --delete origin vX.Y.Z`, and `git tag -d
  vX.Y.Z` if you have a local copy; step 3 created the tag server-side, so you
  may not, and a local one outlives the remote delete if you do). Confirm both
  are gone *on the remote* before re-cutting — `git ls-remote --tags origin
  vX.Y.Z` and the releases page — since a leftover of either collides with the
  new one, and the local tag will not tell you.

## Choosing the version

Pre-1.0, semver is shifted down one level:

- Breaking changes and large internal changes take a **minor** bump. A
  raise to `rust-version` is one of them: 0.10.0 shipped
  [`56e86fa`](https://github.com/akiomik/tears/commit/56e86fa46f9015bcd829836440bd27e71f5bf0ee)
  and the changelog records it as a breaking change, because a build that
  worked stops working.
- Additive features and bug fixes take a **patch**.
- A fix does not wait for a convenient milestone. What this protects is the
  time a user spends carrying a known defect, so the judgment it rules out
  is "the next minor is close, let the fixes accumulate until then".
  Release each one as soon as it is ready.

  That is a question about waiting, not about which release a fix lands in.
  Once main carries breaking changes the next release is a minor whatever
  else goes into it, and a fix written after that point rides the minor
  without being delayed by anything — 0.11.0's two are this case.

Pre-1.0 is also the window to break cleanly: a breaking change that makes
the API right beats a workaround layered over a wrong one, because after 1.0
the mistake is permanent. Plan such breaks early rather than accumulating
them.

## Step 1: the release PR

Branch `chore-bump-version-to-X.Y.Z`, titled `chore: bump version to X.Y.Z`.

### The version

`Cargo.toml`'s `version`, `Cargo.lock` (a build or `cargo package`
regenerates it), and the version pins a reader can copy. Find those rather
than recalling them:

```console
$ grep -rnE '[A-Za-z0-9_-]+ = (\{ version = )?"[0-9.]+"' README.md src/
```

Every line it returns is a dependency line inside a copyable snippet, in
the README or in rustdoc, and nothing checks any of them.

- The `tears` ones belong to this release. They pin `X.Y`, not `X.Y.Z`, so
  what moves them is a change to the series a reader should install:
  normally a minor moves all of them and a patch moves none. Run the grep
  on a patch anyway — it is how one left behind by an earlier minor is
  found, and after an abandoned version (below) the pins may have been
  reverted, in which case the next release is what puts them right whatever
  its number. Missing one leaves a snippet resolving to a crate whose API
  the surrounding text no longer describes.
- The rest are companion pins, and they move when this crate's own
  requirement for that dependency moves, not when its version does.

The README also restates `Cargo.toml`'s `rust-version`, which the MSRV job
reads from that field alone:

```console
$ grep -rniE 'rust[ -]1\.[0-9]' README.md src/
```

Both greps are floors, not proofs: they know the shapes the current
snippets are written in, not every shape one could be. Put a new snippet
beside an existing one, in the same form, rather than expecting either line
to go and find it.

Version strings that describe a *past* release stay as they are:
`CHANGELOG.md`'s entries, the migration guides, the RFCs. That covers a
guide's dependency snippet as well, should one ever carry one, since it
would name the release that guide teaches an upgrade *to*.

### The changelog

Rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD` and open a new empty
`## [Unreleased]` above it. At the bottom, repoint `[unreleased]` at
`compare/vX.Y.Z...HEAD` and add `[X.Y.Z]` pointing at `releases/tag/vX.Y.Z`.
Categories are Keep a Changelog's, as the file's own header says, and a
breaking entry carries its own before/after.

The date is the day of **step 3**, not of this PR. The steps are meant to
run in one sitting, and the date is that day. If publishing slips past the
merge anyway, the date is already shipped: accept it, or land a correction
on main and cut step 3 on that commit instead of the merge commit.

If the release gets a migration guide, this section needs a pointer to it.
Nothing reports that pointer missing, and it is the only one of a guide's
paths an upgrading reader is actually on — so settle the guide question,
below, before treating this section as done.

### The documentation

A release makes documentation untrue rather than merely stale, and the
compiler reports none of it.

- Everything under `Added` that a user can reach is public API. Check that
  the README and the crate-root docs describe it. rustdoc lists every public
  item regardless, so what is lost is discoverability rather than presence:
  an addition only the changelog names is one a reader has to already know
  to look for.

  Feature-gated API is where presence is genuinely at stake, and this step
  cannot fix it. docs.rs builds with `default`, which is empty, so nothing
  under `ws` or `http` reaches it at all
  ([#351](https://github.com/akiomik/tears/issues/351)). Until that is
  settled, prose is the only reach such an addition has — and the
  crate-root `## Optional Features` section is prose docs.rs does render,
  so it is the half of "the README and the crate-root docs" that reaches a
  reader who is already there.
- Everything under `Changed` and `Removed` may have invalidated an example.
  The doctests cover rustdoc and the migration guides; the README's fenced
  blocks are compiled by nothing and have to be read.
- Decide whether the release needs a migration guide.
  [`docs/migrations/README.md`](migrations/README.md) holds the criterion
  and the edits that adding one takes.

### What to run

CI runs on the PR, and it packages the crate. What it never does is report
*which* files were packaged, or run a publish dry run at all:

```console
$ cargo package --list --allow-dirty   # an early look, before committing
$ cargo package --list                 # on the release commit
$ just publish-check                   # on the release commit
```

`cargo package --list` is the only thing that reports what ships. Read the
list, and confirm both directions: everything on it is meant to ship, and
nothing that should ship is missing. `include` in `Cargo.toml` is a list of
patterns, and a pattern can match more than it looks like it does —
[`745e019`](https://github.com/akiomik/tears/commit/745e0192526570dc2fb2f2f2435a7e45d342bd5c)
is the time one did, and no exit status showed it.

`--allow-dirty` buys an early look before the release commit exists, but it
lists the working tree, untracked files included, and that is not what
ships. The listing that counts is the one on the committed tree.

`just publish-check` is the only `cargo publish --dry-run` before the tag.
Run it so that a packaging problem surfaces here rather than inside
`release.yml`, by which point step 3 has made the tag and the release
public. The recipe passes no `--allow-dirty` either, so it wants the
committed tree; run `cargo publish --dry-run --allow-dirty` directly if you
want it sooner.

## Steps 2 and 3: merge, then publish

Find the CI run for the commit you are about to tag — the merge commit,
unless the changelog date slipped and you landed a correction — and confirm
it is green. Neither the PR's run nor the merge button stands in for it:
what blocks a merge and what a release wants green are configured
separately, and a merge commit is a tree that may have been tested nowhere
else.

No publish dry run has seen that tree: CI runs none, and step 1's
`just publish-check` ran on the branch. CI does package it — the doctest
job packages and extracts the crate on main as it does on a PR — but that
is a different check. If anything landed on main between the two, check out
the merge commit and run step 1's two commands on it: `cargo package --list`,
still the only thing that reports what ships, and `just publish-check`. Both
read the working tree, so running them on the branch answers the question
step 1 already answered.

Create the GitHub release on that commit, tagged `vX.Y.Z`, with the
changelog section as the body. Before publishing, confirm the release
points at the commit's SHA — a target left as a branch is one that can move
while you are reading CI, and step 4 would publish a tree whose run you
never saw. Rewrite repo-relative links to absolute URLs pinned at the tag,
so they resolve to the released files rather than to whatever the branch
holds later.

Publishing it creates and pushes the tag, and that push triggers
`release.yml`, which runs `cargo publish --dry-run` and then `cargo publish`
under the toolchain pinned in `rust-toolchain.toml`.

When it finishes, confirm the version on crates.io *and* that its docs.rs
build succeeded. Publishing and documenting are separate, and step 4
reports only the first: the crate can be installable while its published
documentation is still the previous version. A failed build there spends
nothing and is not fixed by cutting another version — take it up at
docs.rs, or in the next release if the cause is in the crate.

## When step 4 fails

Settle one question before doing anything: was the version taken? Do not
infer it from the run — not from its status, not from its log. Look at the
registry, which is the only place that knows:
<https://crates.io/crates/tears/versions>.

**If `X.Y.Z` is listed**, it is spent, and nothing will ever reuse that
number. If what was published is wrong, `cargo yank --version X.Y.Z` takes
it out of new resolutions — but check first what a yank would leave behind,
because every snippet here pins `X.Y`:

- **A healthy earlier `X.Y.*` is still published.** Yank now. `X.Y` falls
  back to it, and nothing new picks up the defect while the fix is written.
- **`X.Y.Z` is the only version in its series.** Ship the fix as the next
  version first, and yank after. Yanking now would leave `X.Y` with nothing
  to resolve to at all.

Yanking needs a crates.io token of your own; publishing through the
workflow leaves nothing logged in on your machine, so `cargo login` before
it.

**If it is not listed**, the version is still free. Re-run the workflow from
the Actions tab. If it fails the same way, the cause is not transient, and
it is one of two:

- **The tagged tree.** No re-run will pass. Land the fix on main and wait
  for that commit's CI exactly as step 3 does — moving the tag re-arms step
  4 immediately, so the precondition is the same one. Then either move the
  tag onto that commit, or abandon the version and release the next patch.

  If you move the tag, the fix is part of `X.Y.Z`, so its changelog entry
  belongs in that section rather than under the `## [Unreleased]` step 1
  opened — and the GitHub release body, which was copied from the section,
  needs the same edit.
- **The publishing credentials.** Either side can be at fault: the
  workflow's own permissions and the action it uses are in the tree, and
  what they authenticate against is held at crates.io. Read the workflow
  first, since that half is in front of you; if it is intact, the fix is at
  crates.io and re-running the same tag picks it up.

Moving the tag is a git operation rather than an edit to the release:

```console
$ git tag -f vX.Y.Z <sha> && git push --force origin vX.Y.Z
```

Afterwards confirm two things on the remote, because either can silently not
happen: that the tag points where you meant (`git ls-remote --tags origin
vX.Y.Z`), and that a run started.

Abandoning the version means cleaning up after one that exists in
`CHANGELOG.md` and as a GitHub release, and never on crates.io. Undo step
1's changelog edits: delete the `## [X.Y.Z] - YYYY-MM-DD` heading, put its
entries back under `## [Unreleased]` where the next release PR will cut
them again, drop the `[X.Y.Z]` link definition, and repoint `[unreleased]`
at the last published tag. Then delete both the GitHub release and the tag
(`git push --delete origin vX.Y.Z`, and `git tag -d vX.Y.Z` if you have a
local copy). Nothing — heading, entry, link, or ref — should send a reader
after a version the registry never had, and a leftover tag is what collides
if the number is ever cut again.

`Cargo.toml` stays where the release PR left it. This branch has given the
number up, so the next release PR moves it past — the registry would still
accept it, but nothing here is waiting on that.

If the abandoned version was a minor, the copyable pins moved with it, and
they need a decision of their own. The next release publishes into the same
`X.Y`, so they come back into line without being touched — but until
something ships into `X.Y`, the install snippet on main names a series the
registry does not have, and that is the one piece of this cleanup a reader
meets first. Leave them if the next release is imminent; revert them if it
is not. (After an abandoned patch there is nothing to decide: the pins
never moved.)
