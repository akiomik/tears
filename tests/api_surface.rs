//! Structural checks of the crate's public API shape, enforcing the "Single
//! Canonical Path" and "Prelude Membership" rules from
//! `docs/api-guidelines.md`. See `docs/testing.md` "Public API Surface Tests"
//! for why these require nightly and are `#[ignore]`d by default.

use std::collections::{HashMap, HashSet};
use std::process::Command;

use public_api::PublicItem;
use public_api::rustdoc_types::Id;
use public_api::tokens::Token;

/// The features a dependant can enable, read from the one place that declares
/// them rather than restated here.
///
/// Not `--all-features`: that turns on `loom-core`, which *removes* code
/// rather than adding it. The gate that would bite carries a `test`
/// (`src/subscription.rs`), and rustdoc JSON builds the library without
/// `cfg(test)`, so nothing is lost from the surface today — but that is a
/// property of one gate's shape, not of the approach. A plain
/// `#[cfg(not(feature = "loom-core"))]` compiles its item away whenever
/// `loom-core` is on, so under `--all-features` it would not reach the
/// surface at all, and the rules below cannot find a violation in an item
/// that is not there: the check would pass by looking at less. Reading the
/// set a dependant can actually enable keeps such an item in view.
///
/// `user_features` and not `build_features`: the public surface is what a
/// dependant can reach, and everything `bench-internals` adds to it arrives
/// through two `#[doc(hidden)]` re-exports in `src/lib.rs`; what they
/// re-export sits under a `pub(crate)` module and is otherwise unreachable.
/// `Cargo.toml` documents the feature as outside the API and not covered by
/// semver.
fn user_features() -> Vec<String> {
    let output = Command::new("just")
        .args(["--evaluate", "user_features"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect(
            "failed to run `just --evaluate user_features`; install just with \
             `cargo install just` (see docs/testing.md)",
        );
    assert!(
        output.status.success(),
        "`just --evaluate user_features` failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let evaluated = String::from_utf8(output.stdout)
        .expect("`just --evaluate user_features` produced non-UTF-8 output");
    // `just --evaluate <var>` prints the bare value and no trailing newline,
    // but trim rather than depend on that.
    let features: Vec<String> = evaluated
        .trim()
        .split(',')
        .map(str::to_owned)
        .filter(|feature| !feature.is_empty())
        .collect();
    assert!(
        !features.is_empty(),
        "`just --evaluate user_features` evaluated to nothing; the surface \
         would be built with default features only",
    );
    features
}

fn build_public_api() -> public_api::PublicApi {
    let mut manifest_path = env!("CARGO_MANIFEST_DIR").to_owned();
    manifest_path.push_str("/Cargo.toml");

    let json_path = rustdoc_json::Builder::default()
        .toolchain("nightly")
        .manifest_path(manifest_path)
        .features(user_features())
        .build()
        .expect(
            "failed to build rustdoc JSON; install a nightly toolchain with \
             `rustup toolchain install nightly`",
        );

    public_api::Builder::from_rustdoc_json(json_path)
        // Blanket/auto-trait impls (`vzip`, `clone_into`, `Freeze`, ...) are
        // rendered once per applicable type and are irrelevant to path
        // reachability; they only add noise to the id-grouping below.
        .omit_blanket_impls(true)
        .omit_auto_trait_impls(true)
        .build()
        .expect("failed to parse rustdoc JSON into a PublicApi")
}

fn is_module(item: &PublicItem) -> bool {
    item.tokens()
        .any(|token| matches!(token, Token::Kind(kind) if kind == "mod"))
}

/// For every publicly reachable item defined directly in a module (i.e. not
/// an associated method/type nested inside a struct or trait), the set of
/// distinct module `Id`s it is reachable through.
///
/// Grouping by `PublicItem::id()` (the underlying rustdoc item id, stable
/// across re-export paths) is what lets this distinguish "one item exposed
/// through two paths" from "two different items that happen to share a
/// name".
fn top_level_reachability(api: &public_api::PublicApi) -> HashMap<Id, HashSet<Id>> {
    let items: Vec<_> = api.items().collect();
    let module_ids: HashSet<Id> = items
        .iter()
        .filter(|item| is_module(item))
        .map(|item| item.id())
        .collect();

    let mut reachable_from: HashMap<Id, HashSet<Id>> = HashMap::new();
    for item in &items {
        if let Some(parent_id) = item.parent_id()
            && module_ids.contains(&parent_id)
        {
            reachable_from
                .entry(item.id())
                .or_default()
                .insert(parent_id);
        }
    }
    reachable_from
}

fn root_module_id(api: &public_api::PublicApi) -> Id {
    api.items()
        .find(|item| is_module(item) && item.parent_id().is_none())
        .map(PublicItem::id)
        .expect("crate root module not found in rustdoc JSON")
}

fn prelude_module_id(api: &public_api::PublicApi, root_module_id: Id) -> Id {
    api.items()
        .find(|item| {
            is_module(item)
                && item.parent_id() == Some(root_module_id)
                && item.to_string().ends_with("::prelude")
        })
        .map(PublicItem::id)
        .expect("tears::prelude module not found in rustdoc JSON")
}

/// docs/api-guidelines.md "Single Canonical Path": an item's conceptual home
/// (crate root, or a public submodule) must be its only reachable path. The
/// prelude is a deliberate, documented exception (see "Prelude Membership"),
/// so a prelude path never counts toward a violation here.
#[test]
#[ignore = "requires a nightly toolchain for rustdoc JSON; see docs/testing.md"]
fn no_public_item_has_two_non_prelude_paths() {
    let api = build_public_api();
    let root_module_id = root_module_id(&api);
    let prelude_module_id = prelude_module_id(&api, root_module_id);

    let violations: Vec<Id> = top_level_reachability(&api)
        .into_iter()
        .filter(|(_id, parents)| parents.iter().filter(|p| **p != prelude_module_id).count() > 1)
        .map(|(id, _)| id)
        .collect();

    assert!(
        violations.is_empty(),
        "found public items reachable through more than one non-prelude path: {violations:?}\n\
         Run `cargo +nightly public-api -ss --features \"$(just --evaluate user_features)\"` \
         to see the same surface this test read (`-ss` drops the blanket and \
         auto-trait impls this test also omits) and close the extra path per \
         docs/api-guidelines.md \"Single Canonical Path\"."
    );
}

/// docs/api-guidelines.md "Prelude Membership": the prelude is a subset of
/// root-level vocabulary, not an alternate placement. Every item reachable
/// through `tears::prelude::*` must also be reachable directly at the crate
/// root (`tears::*`).
#[test]
#[ignore = "requires a nightly toolchain for rustdoc JSON; see docs/testing.md"]
fn prelude_is_a_subset_of_root_level_items() {
    let api = build_public_api();
    let root_module_id = root_module_id(&api);
    let prelude_module_id = prelude_module_id(&api, root_module_id);

    let violations: Vec<Id> = top_level_reachability(&api)
        .into_iter()
        .filter(|(_id, parents)| {
            parents.contains(&prelude_module_id) && !parents.contains(&root_module_id)
        })
        .map(|(id, _)| id)
        .collect();

    assert!(
        violations.is_empty(),
        "found prelude items not reachable at the crate root: {violations:?}\n\
         Every prelude item must also resolve at `tears::*` per \
         docs/api-guidelines.md \"Prelude Membership\"."
    );
}
