//! Structural checks of the crate's public API shape, enforcing the "Single
//! Canonical Path" and "Prelude Membership" rules from
//! `docs/api-guidelines.md`. See `docs/testing.md` "Public API Surface Tests"
//! for why these require nightly and are `#[ignore]`d by default.

use std::collections::{HashMap, HashSet};

use public_api::PublicItem;
use public_api::rustdoc_types::Id;
use public_api::tokens::Token;

fn build_public_api() -> public_api::PublicApi {
    let mut manifest_path = env!("CARGO_MANIFEST_DIR").to_owned();
    manifest_path.push_str("/Cargo.toml");

    let json_path = rustdoc_json::Builder::default()
        .toolchain("nightly")
        .manifest_path(manifest_path)
        .all_features(true)
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
         Run `cargo +nightly public-api --all-features` to see the current surface and \
         close the extra path per docs/api-guidelines.md \"Single Canonical Path\"."
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
