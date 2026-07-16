# API Design Guidelines

This document describes how public API surface decisions are made in this
crate: when a module should be public vs private, when an item should be
promoted to the crate root, and what belongs in the [`prelude`](../src/prelude.rs).
Follow it when adding a new public item so placement stays consistent
instead of being decided ad hoc per PR. "Item" here covers types, traits,
and free functions alike (e.g. `Application`, `SubscriptionSource`, and
`install_panic_hook` are all subject to the same rules).

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

A module can need both at once: stay public for opt-in domain items while
one specific item inside it is root-promoted skeleton vocabulary. In that
case, give the root-promoted item its own private (or `pub(crate)`) inner
submodule — the `runtime::frame_rate` pattern — so the domain module keeps
its public submodules while the promoted item still resolves to exactly
one public path (the crate root), not two.

## Single Canonical Path

Once an item's conceptual home is decided (crate root, or a public
submodule), it should be reachable through exactly one path. Avoid an item
being `pub` inside a `pub mod` *and* re-exported again one level up — that
produces two working `use` paths for the same item with no signal to users
about which one is "the" import, and doubles the surface every future
refactor has to preserve.

A dual path is a defect to fix, not a style choice: close whichever of
the two paths contradicts the "public vs private submodule" call above —
don't leave both open "just in case." Closing a path that's already
shipped is a breaking change, so batch it into a breaking-change release
rather than fixing it piecemeal.

The prelude is a deliberate, documented exception to this rule, not an
oversight — see "Prelude Membership" below. Every prelude item is reachable
both at its canonical path and at `prelude::*` by design; that duplication
is the entire point of a prelude.

`tests/api_surface.rs` checks this rule (and that the prelude stays a subset
of root-level items) mechanically in CI; see `docs/testing.md`.

## Root Promotion Criteria

The crate root (`tears::*`) is for the vocabulary of a minimal
`Application` skeleton — items a new app names directly in `update`,
`view`, or `subscriptions` almost regardless of what the app does.
`Command`, `Subscription`, `Application`, and `Runtime` all pass this bar
directly.

A second, narrower test covers the extension contract behind a skeleton
item. Every `Subscription` is built (via `From`) from an implementation of
`SubscriptionSource`, whose associated `Key` and `key()` method let the
framework construct a `SubscriptionId` — together they are the single general
mechanism the subscription system is built on, not one feature among several.
The dividing question: is the item the one contract all implementations satisfy
(root-eligible), or one implementation among several? `Timer`, `WebSocket`, and
`http::Query` are implementations — each one is still just one option, however
common, so none of them get promoted.

An item that fails both tests does **not** need root promotion just
because it is public: it stays at its domain path
(`tears::subscription::http::Query`), matching the module-visibility call
above. Promoting opt-in feature items would turn the root namespace into a
grab bag instead of a skeleton vocabulary.

Companion types — most commonly the error type returned by a fallible
constructor — share their owner's home even though they fail both tests
individually. `FrameRateError` is at the crate root only because
`FrameRate::new` returns `Result<Self, FrameRateError>`. The "written out
literally" test still applies to companions, but it decides prelude
membership (see "Prelude Membership"), not placement.

## External Crate Re-exports

The tests above assume the item is defined in this crate. A re-exported
external-crate item (e.g. `pub use futures::stream::BoxStream;`) needs a
narrower test instead: promote it only if this crate's own public API
*requires* users to name it, not merely because it appears somewhere in a
signature. `BoxStream` qualifies because implementing `SubscriptionSource`
means writing its return type out — `fn stream(&self) -> BoxStream<'static, Self::Output>`
— so the type is part of the contract of a public trait, not an
implementation detail users can ignore.

Document why next to the `pub use`, the same as any other exception (see
"Applying This"). Don't promote an external type just because it's
convenient or because the crate happens to depend on it; that turns the
root namespace into a proxy for the dependency graph instead of this
crate's own vocabulary.

## Prelude Membership

The prelude is narrower than "everything at the crate root": it is the
subset of root-level vocabulary a minimal skeleton names *by writing the
item out literally*, not through `?` or `.expect(...)`. An item can be
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

When adding a new public item:

1. Decide whether its module should be public (meaningful domain boundary)
   or private (implementation detail) — see "Module Visibility" above.
2. Give it exactly one reachable public path — see "Single Canonical
   Path."
3. Only promote it to the crate root if it passes one of the two tests in
   "Root Promotion Criteria" (or the narrower test in "External Crate
   Re-exports" for re-exported dependency items).
4. Only add it to the prelude if it is written out literally in skeleton
   code, per "Prelude Membership."

When an RFC or design doc introduces new public items, record the intended
path for each one (root, or which domain module) as part of that document
before implementation starts. Deciding placement ad hoc inside the
implementation PR is exactly what this guideline exists to avoid.

These are default rules, not absolutes. If an item needs an exception,
document the reason next to its `pub use` instead of leaving the deviation
silent.
