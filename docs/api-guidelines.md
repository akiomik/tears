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

If you find a `pub mod` whose contents are also re-exported at the parent
level (a dual path), treat that as a defect to fix rather than a style
choice: either close the inner module (`mod`, private, or `pub(crate)`) so
only the outer path remains, or drop the outer re-export so only the inner
path remains. Pick whichever one matches the "public vs private submodule"
call above — don't leave both open "just in case." Closing a path that's
already shipped is a breaking change, so batch it into a breaking-change
release rather than fixing it piecemeal.

The prelude is a deliberate, documented exception to this rule, not an
oversight — see "Prelude Membership" below. Every prelude item is reachable
both at its canonical path and at `prelude::*` by design; that duplication
is the entire point of a prelude.

## Root Promotion Criteria

The crate root (`tears::*`) is for the vocabulary of a minimal
`Application` skeleton — items a new app names directly in `update`,
`view`, or `subscriptions` almost regardless of what the app does.
`Command`, `Subscription`, `Application`, and `Runtime` all pass this bar
directly.

A second, narrower test covers items that don't pass the first one on
their own but define the single extension contract behind a skeleton
item. `subscriptions()` returns `Vec<Subscription<Msg>>`, and every
`Subscription` is built from something implementing `SubscriptionSource`
(via `From`). Most apps never write that `impl` — they reach for a
built-in source like `Timer` instead — so `SubscriptionSource` and the
`SubscriptionId` its `id()` method returns don't pass the "written in
ordinary code" test literally. They're still root-eligible because they
are the *only* general mechanism the subscription system is built on, not
one feature among several, and because a trait and the return type its
own method requires are used together in every implementation — splitting
their paths would add friction with no compensating benefit.

The dividing question for this second test: is the item one of several
interchangeable feature-specific implementations, or is it the single
contract those implementations all satisfy? `SubscriptionSource` /
`SubscriptionId` are the contract. `Timer`, `WebSocket`, and
`http::Query` are implementations of it — each one is still just one
option among several, however common, so none of them get promoted no
matter how frequently apps use that particular source. This test has no
equivalent for `Command`: nothing in this crate implements a trait to
produce a `Command`, so there is no second contract to promote there.

An item that fails both tests does **not** need root promotion just
because it is public. Ask: does an app author write this item's name in
ordinary application code, or only when they opt into one specific feature
(HTTP, WebSocket, a retry policy, a specific subscription source)? Opt-in
feature items stay at their domain path (`tears::subscription::http::Query`)
even though they are fully public — promoting them to root would turn the
root namespace into a grab bag instead of a skeleton vocabulary, and would
put them in front of every unrelated app's autocomplete.

When unsure, check what the item's own domain module already decided about
public vs private submodules above: if the domain module is public because
it's a meaningful boundary, its members normally stay behind that boundary
rather than also being promoted to root.

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
