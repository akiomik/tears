# API Design Guidelines

This document describes how public API surface decisions are made in this
crate: when a module should be public vs private, when a type should be
promoted to the crate root, and what belongs in the [`prelude`](../src/prelude.rs).
Follow it when adding a new public type so placement stays consistent
instead of being decided ad hoc per PR.

## Module Visibility: Public vs Private Submodules

Ask whether the module hierarchy is itself part of what a user of the API
needs to understand.

- **Make the submodule public** when the hierarchy is a meaningful domain
  boundary that only some applications opt into, e.g. `subscription::http`,
  `subscription::websocket`, `subscription::time`. An app that never uses
  HTTP has no reason to see `Query`/`Mutation` at the crate root; the
  `subscription::http` path documents that boundary.
- **Make the submodule private** when the split is purely file
  organization — an implementation detail of how the source happens to be
  laid out, not a concept the user needs to know about. Re-export the
  public items with `pub use` from wherever they conceptually belong.
  `runtime::frame_rate` is private for this reason; `FrameRate` and
  `FrameRateError` are re-exported at the crate root instead.

## Single Canonical Path

Once a type's conceptual home is decided (crate root, or a public
submodule), it should be reachable through exactly one path. Avoid a type
being `pub` inside a `pub mod` *and* re-exported again one level up — that
produces two working `use` paths for the same item with no signal to users
about which one is "the" import, and doubles the surface every future
refactor has to preserve.

If you find a `pub mod` whose contents are also re-exported at the parent
level (a dual path), treat that as a defect to fix rather than a style
choice: either close the inner module (`mod`, private) so only the outer
path remains, or drop the outer re-export so only the inner path remains.
Pick whichever one matches the "public vs private submodule" call above —
don't leave both open "just in case." Closing a path that's already
shipped is a breaking change, so batch it into a breaking-change release
rather than fixing it piecemeal.

## Root Promotion Criteria

The crate root (`tears::*`) is for the vocabulary of a minimal
`Application` skeleton — types a new app names directly in `update`,
`view`, or `subscriptions` almost regardless of what the app does.
`Command`, `Subscription`, `SubscriptionId`, and `SubscriptionSource` all
pass this bar.

A type does **not** need root promotion just because it is public. Ask:
does an app author write this type's name in ordinary application code, or
only when they opt into one specific feature (HTTP, WebSocket, a retry
policy, a specific subscription source)? Opt-in feature types stay at their
domain path (`tears::subscription::http::Query`) even though they are
fully public — promoting them to root would turn the root namespace into a
grab bag instead of a skeleton vocabulary, and would put them in front of
every unrelated app's autocomplete.

When unsure, check what the type's own domain module already decided about
public vs private submodules above: if the domain module is public because
it's a meaningful boundary, its members normally stay behind that boundary
rather than also being promoted to root.

## Prelude Membership

The prelude is narrower than "everything at the crate root": it is the
subset of root-level vocabulary a minimal skeleton names *by writing the
type out literally*, not through `?` or `.expect(...)`. A type can be
correct at the crate root and still not belong in the prelude.

Example: `FrameRate` is in the prelude because skeleton code writes
`FrameRate::new(...)`. `FrameRateError` is not, even though it is
re-exported at the crate root — skeleton code handles it with `?` or
`.expect(...)` without ever spelling the type name, so importing it via
`prelude::*` buys nothing and only adds unused-import noise for apps that
never inspect the error.

Keep the prelude's "What's included" doc comment (`src/prelude.rs`) in
sync with its actual re-exports; don't let it drift into a superset or
subset of what is really exported.

## Applying This

When adding a new public type:

1. Decide whether its module should be public (meaningful domain boundary)
   or private (implementation detail) — see "Module Visibility" above.
2. Give it exactly one reachable public path — see "Single Canonical
   Path."
3. Only promote it to the crate root if it passes the skeleton-vocabulary
   test in "Root Promotion Criteria."
4. Only add it to the prelude if it is written out literally in skeleton
   code, per "Prelude Membership."

These are default rules, not absolutes. If a type needs an exception,
document the reason next to its `pub use` (see the `FrameRate` comment in
`src/lib.rs` for the pattern) instead of leaving the deviation silent.
