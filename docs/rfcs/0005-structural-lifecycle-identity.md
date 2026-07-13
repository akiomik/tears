# RFC 0005: Structural Lifecycle Identity and Composition Scopes

- Status: Draft
- Target: Phase A in 0.10.0 (breaking); Phase B after 0.10.0 (additive)
- Scope: collision-safe subscription identity and hierarchical identity
  namespacing for composed command and subscription lifecycles
- Feature flag: none
- CHANGELOG:
  - Phase A: `Changed` / `Fixed` (`SubscriptionSource::id` replaced by
    associated `Key` / `key`; framework-owned structural `SubscriptionId`;
    pre-hash collision removed; `Copy` removed)
  - Phase B: `Added` (`Subscription::scoped`, `Command::scoped`)

> **Normative boundary.** The non-negotiables, public API and observable
> behavior in sections 1.4, 2–5, and the invariants in section 6 are
> normative. The implementation sketch, performance procedure, examples, and
> alternatives explain or verify those contracts but do not expose an internal
> representation. Phase A and Phase B share one accepted identity model, but
> only Phase A is a 0.10.0 release requirement.

## Summary

`SubscriptionId` currently stores `{ TypeId, u64 }`. A subscription source
hashes its logical identity before constructing the ID, so two unequal logical
keys with the same 64-bit hash become equal to the runtime. The
`SubscriptionManager` then keeps only the first declaration, continues or
restarts the wrong lifecycle, and can retain the wrong message mapping. This is
a correctness failure, not merely an unlikely `HashMap` performance collision.

This RFC replaces the pre-hashed surrogate with a framework-owned erased
structural key. Each `SubscriptionSource` returns an associated `Key`, and
`Subscription::new` combines the concrete source type with that original key.
A subscription identity compares its source type, logical-key type, and
logical-key value. Hashing remains an indexing operation; equality is never
reduced to a hash digest.

Structural local identity is also the foundation for safe feature composition.
When two instances of the same child feature use the same local command or
subscription ID, preserving that ID through `map` aliases their lifecycles in
the root runtime. This RFC therefore defines an ordered, structural scope path
and object-level `scoped` operators that a parent applies at a child composition
boundary.

Delivery is split into two phases:

| Phase | Delivery | Release boundary |
| --- | --- | --- |
| A | Structural `SubscriptionId`, migration, diagnostics, tests, benchmarks | Required for 0.10.0 |
| B | `Subscription::scoped` and `Command::scoped` | Additive; may ship after 0.10.0 |

This RFC does not add reducer composition, preserve per-effect command keys
inside `Command::batch`, or cancel every command below a scope prefix. Those
features depend on this identity model but have separate runtime and lifecycle
contracts.

## 1. Context and constraints

### 1.1 Current subscription identity

The current public shape is:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId {
    type_id: TypeId,
    hash: u64,
}

impl SubscriptionId {
    pub fn of<T: 'static>(hash: u64) -> Self;
}
```

Custom and built-in sources typically use it like this:

```rust
fn id(&self) -> SubscriptionId {
    let mut hasher = DefaultHasher::new();
    self.logical_fields.hash(&mut hasher);
    SubscriptionId::of::<Self>(hasher.finish())
}
```

This performs two conceptually different operations too early:

1. it chooses a hash function and compresses a logical key to 64 bits; and
2. it treats the compressed value as exact identity.

A `HashMap` is collision-safe because it uses `Hash` to find a bucket and `Eq`
to distinguish entries in that bucket. The current API discards the value
needed for the second step, so the manager cannot recover correctness after a
collision.

The impact is observable. `SubscriptionManager::update` builds one desired set
from the IDs, retains the first subscription for each ID, removes running IDs
that are no longer desired, and starts absent IDs. A false equality can
therefore:

- suppress a distinct stream before it starts;
- keep a prior stream when a different one is requested;
- stop or restart the wrong lifecycle; and
- keep the first subscription's message mapping while discarding the second.

### 1.2 Current composition boundary

Both `Subscription::map` and `Command::map` change message types while
preserving lifecycle metadata. That is correct for one feature instance: a
message mapping must not restart a subscription or change a command's
cancellation slot.

It is insufficient when the same child feature is reused:

```rust
let left = child_left
    .subscriptions()
    .into_iter()
    .map(|subscription| subscription.map(ParentMessage::Left));

let right = child_right
    .subscriptions()
    .into_iter()
    .map(|subscription| subscription.map(ParentMessage::Right));

left.chain(right).collect::<Vec<_>>()
```

If both child instances declare the same source type and local key, the root
manager sees one ID. It keeps only the first stream and therefore only the first
message mapping.

Commands have the corresponding global-slot problem. `Command::map` preserves
`CommandId`, so two child instances using `CommandId::new(RequestId::Load)` can
cancel, replace, or suppress each other's effects. This problem exists in the
current manual `Application` composition style; it does not depend on accepting
a future `Reducer` API.

### 1.3 Terms

| Term | Meaning |
| --- | --- |
| Logical key | The original owned value a source or command uses to identify one local lifecycle |
| Local identity | Identity chosen inside one feature, before any parent instance namespace is applied |
| Scope segment | One typed, structural value identifying a composition boundary or child instance |
| Scope path | An ordered sequence of scope segments from outermost to innermost |
| Full identity | Scope path plus local identity; the value used by a runtime registry |
| Duplicate declaration | Two subscriptions with equal full identity in one desired set |
| Shared lifecycle slot | Intentional reuse of one full command identity across updates |

### 1.4 Non-negotiables

**A. Unequal logical keys are not made equal by hash collision.** Hashing may
select a container bucket, but full equality compares structural values.

**B. The concrete subscription source type is a framework-owned namespace.** A
source returns only its associated logical `Key`; `Subscription::new<Source>`
constructs the opaque `SubscriptionId` using `TypeId::of::<Source>()`. Source
implementations cannot choose or substitute this namespace. Therefore two
different concrete source types with equal logical keys produce different
subscription IDs by construction. This preserves the useful part of
`SubscriptionId::of::<Self>` without relying on an implementation convention.

**C. Logical-key type is also part of identity.** Equal-looking values of
different Rust types are distinct. Type erasure must not introduce
cross-type equality.

**D. Existing unscoped command semantics remain unchanged.** `CommandId::new`
keeps the structural equality defined by RFC 0003. `cancellable`,
`cancellable_with`, `cancel`, `map`, and runtime occupancy do not change merely
because the internal structural-key helper may be shared.

**E. Scoping is structural and hierarchical.** Scope segments are not flattened
into a precomputed hash. Segment type, value, boundary, and order participate in
full equality.

**F. Scoping qualifies identity; it is not bulk teardown.** Applying a scope
does not cancel work, create a runtime registry entry, or provide an operation
that selects every descendant of a scope.

**G. Current `Command::batch` granularity remains explicit.** RFC 0003's rule
still applies: child spawn keys are ignored with a warning, child explicit
cancels are folded, and only the top-level batch may own a spawn key. This RFC
does not claim that command scoping solves per-effect batching.

**H. Correctness is not conditional on an allocation benchmark.** Phase A
measures the cost of structural erasure and the loss of `Copy`, but it does not
fall back to collision-unsafe equality because of a benchmark result.

## 2. Decision and identity model

### 2.1 Full identity

The conceptual identities are:

```text
Subscription full identity
  = ScopePath
  + SourceType
  + LogicalKeyType
  + LogicalKeyValue

Command full identity
  = ScopePath
  + LogicalKeyType
  + LogicalKeyValue
```

`ScopePath` is empty for every ID constructed through the Phase A API and for
every command that has not passed through the Phase B `scoped` operator.

Subscription and command identities deliberately remain different public
types. Their registries have different lifecycle semantics:

- subscriptions are declaratively reconciled as a desired set and restarted
  after completion while still desired; and
- commands are imperatively spawned, replaced, kept, or cancelled, with
  deliverability-based occupancy and private output receivers.

An implementation may share an internal erased structural-key primitive. It
must not expose a common public `LifecycleId` or allow a command ID to be used
where a subscription ID is required.

### 2.2 Local equality

For subscription local IDs constructed as `(Source, KeyType, key)`:

```text
(S1, K1, k1) == (S2, K2, k2)
iff
TypeId(S1) == TypeId(S2)
and TypeId(K1) == TypeId(K2)
and k1 == k2 using K1::eq
```

The value comparison is attempted only after the key type matches.

For unscoped command IDs, RFC 0003's equality remains:

```text
(K1, k1) == (K2, k2)
iff
TypeId(K1) == TypeId(K2)
and k1 == k2 using K1::eq
```

The `Hash` implementations feed the same type namespaces, structural values,
scope boundaries, and scope order into the caller's `Hasher`. The normal Rust
contract applies: equal IDs must hash equally. Unequal IDs may have equal hash
output and must remain unequal under `Eq`.

As with every `HashMap` key, this guarantee assumes each logical-key and scope
type obeys the Rust `Eq` / `Hash` laws: equal values hash equally, and equality
and hashing remain stable while the value is used as a registry key. The
framework cannot make a deliberately unlawful `Eq` or nondeterministic `Hash`
implementation correct; such implementations violate the associated `Key`
bounds' semantic contract. This is the same trust boundary as a user-defined
key placed directly in `HashMap`.

### 2.3 Scoped equality

A scope segment contains its erased Rust type and original structural value.
Two scope segments are equal only when both their concrete types and values are
equal.

Scope paths compare element-by-element in order. Applying scopes follows
composition nesting:

```rust
local.scoped(InnerId(7)).scoped(OuterId::Main)
```

has the conceptual path:

```text
[OuterId::Main, InnerId(7)] / local
```

Consequently, when the reversed structural segment sequence differs from the
original:

```text
local.scoped(a).scoped(b) != local.scoped(b).scoped(a)
```

When `a == b`, both paths are `[a, a]` and remain equal. Hash collisions alone
cannot make unequal segment sequences equal.

The implementation may use a flat path, a persistent linked path, or nested
nodes. Segment boundaries must remain structural internally and must not be
replaced by a digest. No path representation or scope introspection is public.

### 2.4 Sharing and independence

Scope makes independence explicit:

```text
PaneId(1) / RequestId::Load != PaneId(2) / RequestId::Load
```

An unscoped command ID remains a root-global lifecycle slot. Reusing the same
full command ID intentionally opts into the existing replacement,
`KeepInFlight`, and explicit-cancel behavior.

Subscriptions do not provide fan-out by duplicate declaration. Equal full IDs
across successive reconciliations express continuity of one desired lifecycle.
Two equal full IDs in the *same* desired set are ambiguous or redundant; the
first wins and a diagnostic is emitted. A genuinely shared subscription should
be hoisted to the common parent and mapped once. Independent child instances
must use distinct scopes.

## 3. Phase A: structural `SubscriptionId`

### 3.1 Public API

Phase A removes public `SubscriptionId` construction and changes the source
contract so the framework receives the original logical key:

```rust
#[derive(Clone)]
pub struct SubscriptionId { /* private */ }

pub trait SubscriptionSource: Send {
    type Output;
    type Key: Eq + std::hash::Hash + Send + Sync + 'static;

    fn stream(&self) -> BoxStream<'static, Self::Output>;
    fn key(&self) -> Self::Key;
}

impl<Msg: 'static> Subscription<Msg> {
    pub fn new<Source>(source: Source) -> Self
    where
        Source: SubscriptionSource<Output = Msg> + 'static;
}

impl std::fmt::Debug for SubscriptionId { /* diagnostic only */ }
impl PartialEq for SubscriptionId { /* source type + erased structural key */ }
impl Eq for SubscriptionId {}
impl std::hash::Hash for SubscriptionId { /* same structural components */ }
impl std::panic::UnwindSafe for SubscriptionId { /* preserved compatibility */ }
impl std::panic::RefUnwindSafe for SubscriptionId { /* preserved compatibility */ }
```

`Subscription::new<Source>` obtains `source.key()` and calls a private
constructor equivalent to:

```rust
SubscriptionId::from_source::<Source, Source::Key>(source.key())
```

Only the framework can select `Source`; the public source implementation returns
the key value but never constructs an ID. The source namespace is therefore
enforced by the generic `Source` already being moved into `Subscription::new`,
not supplied again by user code.

A typical source implementation becomes:

```rust
impl SubscriptionSource for Timer {
    type Output = TimerEvent;
    type Key = NonZeroU64;

    fn stream(&self) -> BoxStream<'static, Self::Output> {
        // ...
    }

    fn key(&self) -> Self::Key {
        self.interval_ms
    }
}
```

`key()` returns an owned value. A source with borrowed or compound identity uses
an owned key type, and a source that caches an instance token returns or clones
that token. The trait does not require `Key: Clone`; each implementation decides
how to produce its owned key. One source implementation has one `Key` type; if
its logical identity has several shapes, it uses an enum rather than changing
the erased key type between instances. This reduced flexibility makes the
source's identity schema explicit and stable.

The bounds serve distinct contracts:

- `Eq + Hash` provides collision-safe registry behavior;
- `'static` permits safe type erasure and downcasting; and
- `Send + Sync` preserves `SubscriptionId`'s cross-thread usability and permits
  an `Arc`-backed implementation without weakening `Subscription`.

In addition to the associated `Key` bounds, `SubscriptionId` preserves its
current `UnwindSafe` and `RefUnwindSafe` auto-trait compatibility. The erased
key does not expose references or mutation through `SubscriptionId`; its
`Eq`/`Hash` implementation is already required to behave as a stable map key.
An implementation may use `AssertUnwindSafe` around the private erased storage
or an equivalent audited representation. This preservation does not add
`UnwindSafe` or `RefUnwindSafe` bounds to logical-key types.

`SubscriptionId` remains re-exported through the same canonical crate-root path
as before, but has no public constructor. This preserves the public identity
type and its diagnostic/trait surface while making namespace construction a
framework responsibility.

Adding `Key` does not by itself make `SubscriptionSource` non-object-safe: a
trait object can spell both associated types, for example
`dyn SubscriptionSource<Output = O, Key = K>`. It is nevertheless a breaking
change for any existing trait-object spelling that specifies only `Output`, and
heterogeneous sources with different key types still need an application-level
erasure wrapper. The repository currently accepts concrete `impl
SubscriptionSource` values and has no `dyn SubscriptionSource` use, so the
stronger construction guarantee is preferred for 0.10.0.

For a public source implementation, its associated `Key` is part of the public
trait surface. Built-in sources should use existing public structural types
where practical (`()`, integers, `String`, `QueryKey`, or tuples of them) and
introduce a small opaque public token type only when an instance identity cannot
be expressed otherwise. This extra API surface is accepted in exchange for
making the source namespace impossible to forge through normal construction.

### 3.2 `Copy` removal

`SubscriptionId` is `Clone` but not `Copy`. Structural erasure may require
shared ownership, and promising `Copy` would constrain the implementation to a
small inline representation that cannot hold arbitrary logical keys.

This is an intentional 0.10.0 breaking change. Internal manager code clones the
opaque ID explicitly. Sources that previously cached a `SubscriptionId` cache
their associated key or instance token instead:

```rust
type Key = InstanceToken;

fn key(&self) -> Self::Key {
    self.instance_token.clone()
}
```

The RFC does not promise `Arc`, one allocation per constructor, pointer-sized
storage, or O(1) cloning. The initial implementation is expected to use shared
erasure and is benchmarked as described in section 7.

### 3.3 No compatibility constructor

`SubscriptionId::of::<T>(u64)` is removed rather than retained as a deprecated
bridge. Its documented input is a precomputed hash; retaining it would preserve
the correctness hole for migrated applications indefinitely.

When a source's *actual* logical key is a `u64`, it passes that value directly:

```rust
type Key = u64;

fn key(&self) -> Self::Key {
    self.connection_number
}
```

When the old `u64` was produced by hashing several fields, the source passes
those fields structurally, usually as a tuple or a private key struct:

```rust
#[derive(Eq, Hash, PartialEq)]
struct WatchKey {
    path: PathBuf,
    recursive: bool,
}

type Key = WatchKey;

fn key(&self) -> Self::Key {
    WatchKey {
        path: self.path.clone(),
        recursive: self.recursive,
    }
}
```

Built-in sources must migrate from `DefaultHasher::finish()` to original
logical components. A per-instance source such as `MockSource` must keep a real
instance token as its associated `Key` rather than treating a timestamp digest
as exact identity.

### 3.4 Debug behavior

The key does not require `Debug`. `SubscriptionId`'s `Debug` output therefore
identifies type namespaces only, for example:

```text
SubscriptionId { source: "tears::subscription::time::Timer", key: "core::num::nonzero::NonZero<u64>", .. }
```

The exact text is not stable. Two unequal keys of the same source and key type
may have identical `Debug` output. Phase B may add scope type names without
printing scope values. Equality and hashing, not `Debug`, define identity.

### 3.5 Duplicate desired IDs

`SubscriptionManager::update` preserves its current deterministic rule: for
equal full IDs in one desired set, keep the first declaration in input order and
ignore later declarations.

The ignored duplicate must be observable through a warning-level tracing event
with target `tears::subscription`. The event must not require the logical key to
implement `Debug` and must not expose key values by default.

The diagnostic's wording, fields, number of events, and rate-limiting policy are
not stable public API. Contract tests assert that a duplicate occurrence is
observable, not a permanent exact event count. Distinct logical keys whose
`Hash` implementations collide are not duplicates and must not warn merely due
to the collision.

## 4. Phase B: composition scopes

### 4.1 Public API

Phase B adds object-level boundary operators:

```rust
impl<Msg: 'static> Subscription<Msg> {
    pub fn scoped<Scope>(self, scope: Scope) -> Self
    where
        Scope: Eq + std::hash::Hash + Send + Sync + 'static;
}

impl<Msg: Send + 'static> Command<Msg> {
    pub fn scoped<Scope>(self, scope: Scope) -> Self
    where
        Scope: Eq + std::hash::Hash + Send + Sync + 'static;
}
```

The methods live on `Subscription` and `Command`, not only on their ID types,
because the parent composition boundary owns the child value and should not
require the child to know its parent instance identity.

No public `ScopeId`, `ScopePath`, or common `LifecycleId` is added. Applications
use their own small typed values such as `PaneId`, `TabId`, or collection item
IDs.

### 4.2 Subscription semantics

`Subscription::scoped(scope)` prepends one segment to the subscription's current
scope path and preserves its source namespace, local key, spawner, and message
type.

Scoping and mapping commute with respect to lifecycle and output:

```rust
subscription.map(f).scoped(PaneId(pane_id))
```

is behaviorally equivalent to:

```rust
subscription.scoped(PaneId(pane_id)).map(f)
```

apart from ordinary closure construction details that are not observable
framework semantics.

The intended manual composition is:

```rust
child
    .subscriptions()
    .into_iter()
    .map(|subscription| {
        subscription
            .map(ParentMessage::Child)
            .scoped(PaneId(child_id))
    })
```

Whether a future reducer API performs this operation automatically is left to
that reducer RFC. It must preserve the identity laws in this RFC.

### 4.3 Command semantics

`Command::scoped(scope)` prepends one scope segment to every command lifecycle
ID present at the call boundary:

- the optional keyed spawn ID and its existing `CancelPolicy`; and
- every explicit ID in the command's cancel list.

It does not change the effect stream, message mapping, redraw directive, timeout
or retry wrappers, cancellation policy, or application output.

This makes local cancellation composable:

```rust
Command::cancel(CommandId::new(RequestId::Load))
    .scoped(PaneId(pane_id))
```

cancels only the `Load` slot in that pane's full identity.

`Command::none().scoped(scope)` is lifecycle-inert because there is no spawn key
or explicit cancel to qualify.

Like the existing command modifiers, call order is meaningful. `scoped` wraps
metadata present when it is called:

> **Ordering requirement.** `scoped` is a boundary operation over existing
> lifecycle metadata, not a persistent mode inherited by later modifiers.
> Calling `cancellable` after `scoped` installs a new root-global spawn key. The
> Phase B implementation must place this warning prominently in the rustdoc for
> `Command::scoped`, `Command::cancellable`, and `Command::cancellable_with`,
> with examples of both orders. A cross-link hidden only in an RFC or general
> composition guide is insufficient.

```rust
work.cancellable(CommandId::new(RequestId::Load))
    .scoped(PaneId(pane_id));
```

uses the pane-scoped key. Conversely:

```rust
work.scoped(PaneId(pane_id))
    .cancellable(CommandId::new(AppRequestId::GlobalRefresh));
```

applies the later, root-global key to the command, consistent with RFC 0003's
last-call-wins rule. Explicit cancel IDs already wrapped by the earlier
`scoped` call remain scoped.

Composition helpers should apply child scope after constructing the child
command and may place `map` on either side:

```rust
child.update(message)
    .map(ParentMessage::Child)
    .scoped(PaneId(child_id))
```

### 4.4 `Command::batch` boundary

This RFC preserves RFC 0003 INV-11:

- child explicit cancel lists are folded into the batch;
- child spawn keys are ignored with a warning; and
- a key applied to the resulting batch identifies the whole top-level batch.

Consequently:

```rust
Command::batch([
    left_command.scoped(PaneId::Left),
    right_command.scoped(PaneId::Right),
])
```

preserves scoped explicit cancels but does **not** preserve either child spawn
key. Applying `scoped` to the resulting batch scopes its folded explicit
cancels and any top-level key already present at that call boundary.

Preserving independently keyed child effects requires the runtime lowering to
spawn multiple keyed tasks from one returned command. That is RFC 0003's
deferred per-effect cancellation work and is not silently added here.

### 4.5 Scoping is not teardown

`scoped(PaneId(7))` qualifies IDs; it does not create an ownership handle or a
prefix-cancellation command.

A future operation such as `Command::cancel_scope(PaneId(7))` would need to
decide at least:

- whether selection matches one segment or a complete prefix path;
- whether lookup scans current entries or maintains a secondary index;
- ordering relative to same-update spawns and explicit ID cancels;
- treatment of running and finished-but-buffered command output;
- whether subscriptions participate and how declarative re-evaluation interacts
  with teardown; and
- whether a later child reusing the same scope can observe stale teardown state.

Those decisions belong to a separate RFC. The structural path defined here is
forward-compatible with such work but does not pre-accept its API or behavior.

### 4.6 Residual composition risk

Phase B provides the vocabulary to express correct instance-local identity, but
manual scoping alone does not make incorrect composition unrepresentable. A
parent can omit `scoped`, reuse the same scope value for two child instances, or
attach a local command key after the scope boundary. In each case the code
compiles and may alias lifecycle slots.

Subscriptions partially expose this mistake through the duplicate warning when
two equal full IDs appear in one desired set. Commands cannot generally warn:
an unscoped or reused full `CommandId` is indistinguishable from the intentional
root-global shared slot supported by RFC 0003, so replacement, suppression, or
cross-cancellation may be silent.

Making child-instance scoping correct by construction requires a future
composition layer to own the boundary and apply the instance scope
automatically, such as collection/reducer composition keyed by the child
instance ID. This RFC intentionally defers that API. Phase B is therefore an
explicit manual primitive and a prerequisite for that stronger design, not the
final construction guarantee for composed applications.

## 5. Compatibility and delivery

### 5.1 Phase A compatibility

Phase A is intentionally breaking and belongs in 0.10.0:

| Existing contract | Phase A change |
| --- | --- |
| `SubscriptionId::of::<Source>(hash)` | Removed with no public replacement; `Subscription::new<Source>` constructs the ID |
| `SubscriptionSource::id() -> SubscriptionId` | Replaced by associated `type Key` and `key() -> Self::Key` |
| `dyn SubscriptionSource<Output = O>` spelling | Must also specify `Key = K`; heterogeneous key types require separate erasure |
| `SubscriptionId: Copy` | Removed; use `Clone` |
| `SubscriptionId: Send + Sync + UnwindSafe + RefUnwindSafe` | Preserved and pinned by positive compile-time assertions |
| Caller chooses and freezes a hash digest | Framework hashes the original structural key |
| Key needs no traits after hashing | Logical key requires `Eq + Hash + Send + Sync + 'static` |

Every built-in `SubscriptionSource`, public example, doctest, benchmark, and
test implementation must migrate in the same 0.10.0 change. The changelog must
show before-and-after custom source code and call out the loss of `Copy`.

### 5.2 Phase B compatibility

Phase B is additive. Unscoped subscriptions and commands keep an empty scope
path and retain Phase A / RFC 0003 behavior. No application is automatically
scoped based on closure type, message mapper, vector position, or memory
address. As specified in section 4.6, forgetting or reusing a manual scope
remains valid code, and command aliasing can be silent until a future
composition API owns this boundary.

Deferring Phase B implementation does not require another breaking change. The
opaque public ID representations permit a later structural scope node without
changing the Phase A source-key or subscription-construction signatures.

### 5.3 RFC status across phases

Acceptance approves both phases' semantics. Implementation tracking must state
phase status explicitly. Implementing Phase A is sufficient for the 0.10.0
release gate but is not evidence that `scoped` APIs or full reducer composition
have shipped.

The RFC should not be marked simply `Implemented` while Phase B is absent;
project tracking may use an implementation checklist or a qualified status such
as `Partially Implemented (Phase A)` until both public phases ship.

## 6. Invariants and contract tests

### 6.1 Identity invariants

- **INV-1: structural subscription equality.** Subscription IDs are equal only
  when source type, logical-key type, logical-key value, and scope path are
  equal.
- **INV-2: collision safety.** Unequal logical keys remain unequal even when
  they feed identical bytes or a constant value into every `Hasher`.
- **INV-3: source namespace.** `Subscription::new<Source>` constructs the ID
  using `TypeId::of::<Source>()` and `Source::Key`; source implementations return
  only the key and cannot select the source namespace. Equal keys from different
  concrete source types are therefore different IDs by construction.
- **INV-4: key type namespace.** Equal-looking keys of different concrete Rust
  types are different IDs.
- **INV-5: hash consistency.** Assuming logical key and scope types obey the
  Rust `Eq` / `Hash` laws, equal IDs hash equally. Hash equality does not imply
  ID equality.
- **INV-6: owned erasure.** IDs do not borrow non-`'static` source state.
- **INV-7: public shape compatibility.** `CommandId` and `SubscriptionId` remain
  distinct public types even if they share internal machinery.
  `SubscriptionId` remains `Send + Sync + UnwindSafe + RefUnwindSafe`; only
  `Copy` is intentionally removed from its existing auto/marker trait surface.

### 6.2 Subscription manager invariants

- **INV-8: collision-independent reconcile.** Two unequal, hash-colliding
  desired IDs may both run, stop, and restart independently.
- **INV-9: mapping preservation.** Hash-colliding subscriptions retain their
  own spawn closures and message mappings; output from each reaches the expected
  parent message variant.
- **INV-10: duplicate first-wins.** Equal full IDs in one desired set retain the
  first declaration in input order.
- **INV-11: duplicate observability.** Ignoring a duplicate full ID is
  observable at warning level without requiring key values to implement
  `Debug`.
- **INV-12: lazy spawn.** Structural ID comparison does not invoke a discarded
  duplicate's stream spawner or recreate a continuing subscription's stream.
- **INV-13: restart contract unchanged.** A finished subscription that remains
  desired under the same full ID restarts on the next re-evaluation.

### 6.3 Scope invariants

- **INV-14: typed and tagged scope segments.** Scope segment type and value both
  participate in equality, and the framework's scope node kind is distinct from
  every user local-key shape. Embedding the same values in an unscoped tuple or
  struct cannot forge a scoped identity.
- **INV-15: ordered nesting.** Reversing scope application reverses the path.
  The full identity is distinct exactly when the reversed structural segment
  sequence differs from the original; reversing two equal segments preserves
  equality.
- **INV-16: independent child instances.** Equal local IDs under unequal scope
  paths do not alias in subscription or command registries.
- **INV-17: map propagation.** `Subscription::map` and `Command::map` preserve
  full identity; applying `map` immediately before or after the same scope is
  behaviorally equivalent.
- **INV-18: command metadata coverage.** `Command::scoped` qualifies both the
  keyed spawn ID and every explicit cancel ID present at the call boundary.
- **INV-19: command cancellation isolation.** Cancelling or replacing a full ID
  under one scope cannot affect an equal local ID under a different scope.
- **INV-20: batch compatibility.** Scoping does not bypass RFC 0003's batch
  boundary: child keys are still ignored, while scoped explicit cancels fold.
- **INV-21: no implicit teardown.** Dropping a value returned by `scoped` or
  omitting one scoped command does not issue prefix cancellation beyond the
  lifecycle's existing ID-specific rules.

### 6.4 Required tests

Phase A unit tests:

- same source type, key type, and equal value compare equal;
- unequal values compare unequal;
- different concrete source types with the same associated `Key` type and value
  compare unequal;
- different key types compare unequal;
- two unequal values with constant `Hash` output have equal computed hashes
  under a test hasher but unequal IDs;
- cloning preserves equality and hashing;
- `Debug` reports type clues without requiring or exposing values;
- a positive compile-time assertion requires
  `SubscriptionId: Send + Sync + UnwindSafe + RefUnwindSafe`; and
- public API surface tracking records removal of `Copy`, removal of public ID
  constructors, and addition of `SubscriptionSource::Key` / `key`.

Phase A manager tests:

- two constant-hash logical keys start two streams;
- both streams deliver through their own message mappings;
- removing one aborts only that stream;
- a completed colliding stream restarts without replacing the other;
- an equal duplicate keeps the first, does not invoke the second spawner, and
  produces an observable warning; and
- a hash collision without equality does not produce the duplicate warning.

Phase B scope tests:

- same local subscription ID in two pane scopes starts two streams;
- removing one pane stops only its scoped stream;
- same local command ID in two pane scopes does not cross-cancel, replace, or
  trigger `KeepInFlight` suppression;
- a scoped explicit cancel affects only its matching scoped spawn;
- two unequal values of one scope type whose `Hash` implementation writes a
  constant value still start independent subscriptions;
- the same constant-hash scope values keep command replacement, suppression,
  and explicit cancellation isolated;
- an unscoped local key containing the same values as a scope and local key
  (for example `(PaneId(1), RequestId::Load)`) is unequal to the framework
  identity produced by `.scoped(PaneId(1))`, for both subscriptions and
  commands;
- scope type differences affect equality;
- reversing two unequal scope segments changes identity, while reversing two
  equal segments preserves it;
- `map`/`scoped` placement preserves lifecycle behavior;
- `cancellable(local).scoped(pane)` produces a pane-scoped spawn key;
- `scoped(pane).cancellable(global)` produces a root-global spawn key;
- in a mixed command, explicit cancels present before `scoped(pane)` remain
  pane-scoped while a spawn key attached by a later `cancellable(global)` is
  root-global;
- batch tests retain RFC 0003's ignored-child-key warning and folded-cancel
  behavior.

Tests involving async tasks use deterministic synchronization rather than
sleeps.

## 7. Performance evaluation

The current `SubscriptionId` is two inline machine values and `Copy`.
Structural erasure is expected to add allocation, indirection, dynamic equality,
and explicit clones. Subscription reconciliation is a per-message hot path, so
the change must be measured even though collision-safe equality is mandatory.

The existing Criterion subscription benchmark remains the primary comparison:

- `subscription_reconcile_steady`: identical desired IDs while tasks continue;
- `subscription_reconcile_churn`: disjoint desired sets that abort and spawn;
  and
- counts `1`, `8`, `64`, and `256`.

The Phase A benchmark migration must construct IDs through the same framework
path as real sources (`Subscription::new<Source>` calling `Source::key`) so ID
allocation is not accidentally excluded. Record before/after distributions for
time and, where practical, allocation counts.
The benchmark notes must distinguish:

- ID construction cost;
- cloning into desired/running collections;
- structural hashing and equality in steady reconcile; and
- task abort/spawn cost that dominates churn at larger counts.

If allocation dominates, permitted follow-ups include source-owned shared key
values, framework-side sharing of erased keys, small-object optimization, or
reducing manager clones. They must preserve every identity invariant and remain
internal. Reintroducing a pre-hashed equality surrogate is not a permitted
optimization.

Phase B adds a separate scoped steady-state case before its implementation is
declared complete. It should compare empty, one-segment, and representative
nested scope paths without making a particular path representation public.

## 8. Implementation guide

This section is non-normative except where it cites an invariant.

### 8.1 Erased structural key

The existing `CommandId` implementation provides the baseline:

```rust
trait ErasedKey: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn erased_type_id(&self) -> TypeId;
    fn type_name(&self) -> &'static str;
    fn eq_erased(&self, other: &dyn ErasedKey) -> bool;
    fn hash_erased(&self, state: &mut dyn Hasher);
}
```

A private `StructuralKey` can own `Arc<dyn ErasedKey>` and implement `Clone`,
typed equality, hashing, and diagnostic type names once. `CommandId` can migrate
to that helper without changing RFC 0003 behavior. `SubscriptionId` adds the
`TypeId` selected by `Subscription::new<Source>` around the same key primitive.
The private subscription-ID constructor accepts `Source::Key`; it is not
reachable from downstream source implementations.

A bare `Arc<dyn ErasedKey>` does not preserve `UnwindSafe` or `RefUnwindSafe`.
The `SubscriptionId` representation must therefore wrap its private erased
storage in `AssertUnwindSafe` or use an equivalent representation that
preserves both marker traits. This is an intentional compatibility assertion:
the ID owns its key, exposes no references into it, and requires stable `Eq` and
`Hash` behavior. Do not solve this by adding unwind-safety bounds to the shared
`ErasedKey` trait, because that would unnecessarily narrow `CommandId` and the
logical-key API. A positive compile-time assertion pins the resulting public
contract.

Scope nodes should use the structural primitive but carry an internal node tag
or representation that preserves path boundaries. A user-created tuple that
happens to contain the same values is not implicitly equal to framework scope
structure.

### 8.2 Manager migration

Removing `Copy` requires deliberate ownership in `SubscriptionManager::update`:

- insert cloned IDs into `HashSet` or restructure around owned map entries;
- avoid cloning keys more often than the chosen collection ownership requires;
- preserve desired input order and first-wins behavior;
- clone finished IDs rather than dereferencing them; and
- keep stream spawners lazy.

The manager should not compare diagnostic strings, `Arc` addresses, or cached
hashes for equality.

### 8.3 Built-in source migration

Each built-in source must expose its actual lifecycle inputs as its associated
`Key`:

- `Timer`: interval value;
- terminal and signal sources: the semantic configuration or signal kind;
- WebSocket: connection inputs that currently participate in its `Hash`;
- HTTP `Query`: client identity, response type through `Self`, and structural
  `QueryKey` inputs;
- `MockSource`: one clone-stable per-instance token stored by the source; and
- benchmark/test sources: their caller-controlled logical key.

Do not mechanically replace `id()` with `type Key = u64` and return
`hasher.finish()` when that value is still a digest. That would use the new trait
shape without fixing the old semantics.

### 8.4 Delivery sequence

Phase A:

1. Add or extract the internal structural-key helper and keep `CommandId`
   contract tests green.
2. Replace the `SubscriptionId` representation, make construction private, and
   change `SubscriptionSource::id` to associated `Key` / `key`.
3. Adapt `SubscriptionManager` to non-`Copy` IDs and add duplicate diagnostics.
4. Migrate every built-in source, test, example, doctest, and benchmark to
   original logical keys.
5. Add unit and manager collision regression tests, including positive
   `Send + Sync + UnwindSafe + RefUnwindSafe` assertions.
6. Run API-surface, full feature, MSRV, lint, documentation, and benchmark
   verification.
7. Add the 0.10.0 changelog migration example.

Phase B:

1. Add the internal ordered scope node/path without changing unscoped equality.
2. Add `Subscription::scoped` and its composition tests.
3. Add `Command::scoped`, covering spawn keys and explicit cancels.
4. Pin scope-hash collision safety, modifier ordering (including mixed scoped
   cancels/root-global spawn metadata), and RFC 0003 batch compatibility.
5. Add scoped command runtime tests and scoped benchmark cases.
6. Document manual child composition, the silent aliasing residual risk, the
   modifier-order warning in method rustdoc, and the per-effect batch limitation.

## 9. Alternatives considered

### Keep `{ TypeId, u64 }` because collisions are unlikely

Rejected. Probability does not repair the equality contract. A collision causes
the manager to control and route the wrong lifecycle, and callers cannot detect
or recover from it after the original key has been discarded.

### Keep `of` as a deprecated compatibility constructor

Rejected. Existing examples explicitly teach callers to pass a digest. A
deprecated escape hatch would keep producing false equality and make the new
correctness guarantee conditional on voluntary migration. 0.10.0 already
permits the necessary breaking change.

### Let callers choose `SubscriptionId::new::<Source>(key)`

Rejected. A caller-selected generic parameter is only a declared namespace; it
does not guarantee the concrete `SubscriptionSource` type. Two unrelated source
implementations could accidentally choose the same shared namespace and key.
Framework-owned construction from `Source::Key` removes that choice entirely.

### Infer the namespace from `SubscriptionId::new(&source, key)`

Rejected after initially being selected. Passing `self` is the easiest and
documented path, but a source implementation can still pass another
`SubscriptionSource` value and silently choose its type namespace. This is a
useful ergonomic guardrail, not a construction guarantee. Returning an
associated `Key` and letting `Subscription::new<Source>` construct the ID makes
the concrete source namespace a consequence of the type system instead of a
trait convention.

### Widen the digest to 128 or 256 bits

Rejected. A larger digest reduces probability but retains the same semantic
flaw. The framework already needs `Eq + Hash` for structural `CommandId`, so it
has an established collision-safe model.

### Accept only a caller-provided `u64` logical ID

Rejected as the general API. It avoids framework-side pre-hashing only when the
domain truly has a canonical `u64`. Most sources have tuple- or struct-shaped
identity, and forcing manual numbering moves collision and namespace management
to the application.

### Use only the logical-key type as the subscription namespace

Rejected. Two independent source types commonly use the same primitive or
shared key type. Keeping the source type preserves the current useful
namespace and avoids requiring every source to invent a private wrapper solely
for separation.

### Require `Debug` and print key values

Rejected. Values are unnecessary for equality and may contain sensitive or
high-cardinality data. Requiring `Debug` would also reject otherwise valid
identity types. Type-only diagnostics match `CommandId`.

### Preserve `Copy` with a small inline erased representation

Rejected as a public constraint. Arbitrary owned structural values cannot in
general fit a stable small inline layout. Internal optimization remains possible
without promising `Copy`.

### Drop `UnwindSafe` and `RefUnwindSafe`

Rejected. Both marker traits are part of the current public type's auto-trait
surface, and structural erasure alone is not a reason to remove them. The
opaque ID owns its stable map key and exposes no inner references, so the
private erased storage can be wrapped in `AssertUnwindSafe` without adding
unwind-safety bounds to every logical key. Positive compile-time assertions
keep this compatibility visible even though the structural API-surface test
does not enumerate auto traits.

### Encode pane identity directly in every child ID enum

Possible today but rejected as the composition model. It forces a reusable child
feature to know parent instance identity and makes every command and subscription
key repeat the same namespace. A parent-owned object-level boundary is the
composable abstraction.

### Derive scope from `map` closures or message variants

Rejected. Closure types identify code locations, not dynamic child instances;
two panes commonly use the same mapper. Message variants distinguish routing
cases but not collection elements or repeated instances. Automatic derivation
would be unstable and incomplete.

### Add scope only to subscriptions

Rejected at the identity-model level. The current subscription failure is more
immediate, but `CommandId` has the same multiple-child aliasing problem. Defining
one scope algebra now prevents incompatible command and subscription composition
rules. Phase delivery may still be staged.

### Unify `CommandId` and `SubscriptionId` publicly

Rejected. Sharing equality machinery does not make lifecycle semantics the
same. A common public type would allow accidental cross-domain reuse and make
future APIs harder to explain.

### Include per-effect `Command::batch` preservation

Deferred. It is necessary for complete reducer effect composition, but it
changes runtime lowering and keyed task granularity. RFC 0003 intentionally
deferred it. RFC 0005 documents the boundary rather than expanding into that
runtime redesign.

### Include prefix cancellation and feature teardown

Deferred. Identity namespacing is a prerequisite but not an implementation of
ownership or selection. Prefix teardown needs its own registry, ordering, and
buffered-output decisions.

### Require Phase B implementation before 0.10.0

Rejected as a release gate. Phase A is the breaking correctness fix that must
land while 0.10.0 can remove `of` and `Copy`. Phase B is additive and benefits
from further manual composition examples and per-effect command design. Its
semantics are accepted now so Phase A does not close off the future path.

## 10. Non-goals and follow-up work

This RFC does not:

- add `Reducer`, `Store`, lens, or optics APIs;
- decide how a future reducer derives or applies child scopes automatically;
- preserve independently keyed command children through `Command::batch`;
- add prefix lookup, `cancel_scope`, or subscription subtree teardown;
- merge or fan out duplicate subscription message mappings;
- unify the command and subscription managers;
- change command occupancy, output suppression, or cancellation policies;
- add subscription restart backoff or a restart safety fuse;
- add runtime channel bounds, backpressure, or load-control policy; or
- expose scope paths or erased key internals publicly.

Follow-up order is:

1. implement Phase A for 0.10.0;
2. evaluate and, when scheduled, implement Phase B additively;
3. design per-effect command cancellation/batch composition;
4. use scoped identity and deterministic effect testing as inputs to the
   reducer composition RFC; and
5. consider prefix teardown only with a concrete ownership use case.

## 11. References

- `src/subscription/core.rs`
- `src/subscription.rs`
- `src/command/cancellation.rs`
- `src/command/core.rs`
- `src/runtime/keyed_commands.rs`
- `benches/subscription.rs`
- `docs/rfcs/0001-http-module-redesign.md`
- `docs/rfcs/0003-command-cancellation.md`
- `docs/api-guidelines.md`
- TCA reducer `Scope`, identified collection composition, and effect
  cancellation IDs
