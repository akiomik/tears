//! The superseded topology, asserted absent.
//!
//! RFC 0014 §14.6's second condition is that the old architecture is not
//! constructible. Deleting it is what makes that true; this is what keeps it
//! true, because the failure mode is not a deletion that did not happen — it
//! is a *reintroduction* months later, by someone restoring a name from a
//! stale branch or reaching for a familiar identifier that no longer means
//! anything.
//!
//! Two halves, at two granularities.
//!
//! The public half — that `FrameRate`, `FrameRateError` and the two retired
//! configuration controls are unreachable from outside the crate — is
//! enforced by the compiler for anyone who tries to name them, and by
//! `api_surface`'s reachability check for anyone who re-exports them. It
//! needs no row here.
//!
//! The internal half does. A name can come back inside `src/` without any
//! public surface moving: a re-added `FrameScheduler`, a second capacity knob
//! called `keyed_channel_capacity`, a capacity-wait event that starts
//! labelling a lane `"shared"` again. Each would be a topology the RFC
//! supersedes, growing back below the API where no signature check looks. So
//! this scans the source itself.
//!
//! **What a row asserts is the identifier's absence, not a behaviour.** That
//! is a weaker claim than the rest of the suite makes, and deliberately so:
//! it is a tripwire on the vocabulary, and its value is that it fails at the
//! moment the name reappears rather than at the moment the behaviour behind
//! it does. A row that needs to allow the name back is a decision to record,
//! not a test to loosen quietly.

#![expect(
    clippy::panic,
    reason = "a scan that cannot read the tree it scans has no verdict to \
              report, so it fails loudly rather than passing vacuously"
)]

use std::fs;
use std::path::{Path, PathBuf};

/// Every `.rs` file under `src/`.
fn source_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()));
        for entry in entries {
            let path = entry.expect("failed to read a directory entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                out.push(path);
            }
        }
    }

    let mut files = Vec::new();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    walk(&root, &mut files);
    assert!(
        files.len() > 20,
        "the scan found {} files, which is too few to be scanning the crate",
        files.len()
    );
    files
}

/// Fails naming every `src/` line that contains `needle`.
///
/// The reason is carried in the failure rather than in a comment, so whoever
/// trips the row reads why the name is retired at the moment they trip it.
#[track_caller]
fn assert_absent_from_src(needle: &str, reason: &str) {
    assert_absent_from(&source_files(), needle, reason);
}

/// The same check over a named subset, for needles whose retired meaning is
/// local to one module and whose spelling is ordinary elsewhere.
#[track_caller]
fn assert_absent_from(files: &[PathBuf], needle: &str, reason: &str) {
    let mut hits = Vec::new();
    for path in files {
        let text = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        for (number, line) in text.lines().enumerate() {
            if line.contains(needle) {
                hits.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    number + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "`{needle}` is back in the source, and it should not be: {reason}\n{}",
        hits.join("\n")
    );
}

/// Frame pacing: the scheduler, its gate, and the rate that configured it.
///
/// RFC 0014 §6.3 removes configured pacing outright — render cadence is
/// pass-bounded now — and §9 row 4 takes the wall-clock reads it needed with
/// it. There is no frame period left for anything to be scheduled against.
#[test]
fn frame_pacing_has_not_grown_back() {
    let reason =
        "frame pacing is removed; render cadence is pass-bounded (RFC 0014 §6.3, §9 row 4)";
    assert_absent_from_src("FrameScheduler", reason);
    assert_absent_from_src("FrameRate", reason);
    assert_absent_from_src("frame_rate", reason);
    assert_absent_from_src("PendingWork", reason);
}

/// Per-key private channels and the multiplexer that pulled from them.
///
/// One lane, origin-tagged, carries every producer's message output now
/// (RFC 0014 §3.1). `keyed_channel_capacity` had nothing left to size and
/// `AppInputs`/`KeyedCommands` nothing left to multiplex.
#[test]
fn the_private_keyed_channels_have_not_grown_back() {
    let reason = "producer output travels one shared data lane (RFC 0014 §3.1, §9 row 2)";
    assert_absent_from_src("keyed_channel", reason);
    assert_absent_from_src("AppInputs", reason);
    assert_absent_from_src("KeyedCommands", reason);
    assert_absent_from_src("app_channel_capacity", reason);
}

/// The observability vocabulary the retired channels owned.
///
/// RFC 0014 §9 row 9 keeps every field *name* — a renamed telemetry field
/// breaks dashboards silently, off the compiler's path — and retires two
/// *values* of one field with the channels they named. `shared_pending` is
/// therefore expected to still be here, reading as the data lane's residual
/// occupancy; what must not come back is a lane labelled `"shared"` or
/// `"keyed"`.
#[test]
fn the_retired_channel_labels_have_not_grown_back() {
    let reason = "the capacity-wait `channel` field takes one value, \"data\" (RFC 0014 §9 row 9)";

    // The variants, crate-wide: a second one is a second lane, wherever it
    // is declared.
    assert_absent_from_src("Channel::Shared", reason);
    assert_absent_from_src("Channel::Keyed", reason);

    // The emitted strings, only where labels are emitted. Crate-wide these
    // two words are ordinary English — `.expect("keyed")` in a test says
    // nothing about a lane — so widening this row would make it fail for
    // reasons that are not the one it is named for.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime");
    let emitters = vec![root.join("load.rs"), root.join("channel.rs")];
    for path in &emitters {
        assert!(path.exists(), "{} moved; retarget this row", path.display());
    }
    assert_absent_from(&emitters, "\"shared\"", reason);
    assert_absent_from(&emitters, "\"keyed\"", reason);
}

/// The command layer's two readings of one lowering.
///
/// `into_execution_parts` existed so the superseded runtime could fold a
/// command into a single stream and key the fold. With one consumer there is
/// one reading, and a second one reappearing would mean two consumers
/// disagreeing about what a command means again (RFC 0008 INV-T3).
#[test]
fn the_second_lowering_reading_has_not_grown_back() {
    assert_absent_from_src(
        "into_execution_parts",
        "the lowering boundary has one reading, `into_kernel_parts` (RFC 0008 INV-T3)",
    );
}

/// The `Application`-facing surface that keying a batch or a quit needed.
///
/// A spawn key attaches to a single effect carrier, which is why
/// `cancellable` lives on `EffectCommand` alone; a `Command`-level key is the
/// shape RFC 0014 §3.4 declares not constructible.
#[test]
fn the_command_level_spawn_key_has_not_grown_back() {
    assert_absent_from_src(
        "cancellation.key",
        "a spawn key attaches to one effect carrier, not to a command (RFC 0014 §3.4)",
    );
}
