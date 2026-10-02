//! The live-instance report and the reconciliation that reads it (RFC 0014
//! §2.5, INV-RC3/INV-RC3a).
//!
//! A reducer reports the occupancies its state holds through
//! [`Reducer::instances`](super::Reducer::instances) as (qualified path,
//! identity) pairs. After every update, the kernel and the store compare the
//! report with the one before and merge into the command they dispatch for
//! that update one teardown per path that disappeared, except a path under
//! another that disappeared too, which that one's teardown selects.

use std::collections::HashSet;
use std::hash::Hash;

use crate::command::Command;
use crate::structural_key::{ScopePath, StructuralKey};

use super::Reducer;
use super::collection::{InstanceId, Keyed, ScopeValue, Slot};

/// One report: every occupancy a state holds, with the path it is reported
/// under, in the order the reducers reported them.
pub type Report = Vec<(ScopePath, InstanceId)>;

/// What a reducer reports its occupancies through.
///
/// It is append-only and opaque: an implementation can descend under a
/// segment and report the rows of a [`Keyed`] or the occupant of a [`Slot`],
/// and it cannot read or remove what was reported. What it reports must be
/// the collections the state holds: a `Keyed` built inside `instances`
/// carries identities no update has seen, so reporting one tears down every
/// row it stands for on each update.
///
/// What a reducer reports through it is stated on
/// [`Reducer::instances`](super::Reducer::instances).
pub struct Instances<'a> {
    prefix: ScopePath,
    report: &'a mut Report,
}

impl Instances<'_> {
    /// Reports what `visit` reports, one boundary down, under `seg`.
    ///
    /// This is what a [`scope`](super::ReducerExt::scope) boundary does: it
    /// adds a segment and no occupancy of its own.
    pub fn scoped<Seg>(&mut self, seg: Seg, visit: impl FnOnce(&mut Instances<'_>))
    where
        Seg: Eq + Hash + Send + Sync + 'static,
    {
        let mut child = Instances {
            prefix: self.prefix.child(StructuralKey::new(seg)),
            report: &mut *self.report,
        };
        visit(&mut child);
    }

    /// Reports each row of `rows` under its key, and what `visit` reports
    /// for that row beneath it.
    pub fn keyed<K: ScopeValue, V>(
        &mut self,
        rows: &Keyed<K, V>,
        mut visit: impl FnMut(&V, &mut Instances<'_>),
    ) {
        for (key, value, id) in rows.occupancies() {
            self.occupy(StructuralKey::new(key.clone()), id, |out| visit(value, out));
        }
    }

    /// Reports the occupant of `slot`, if there is one, under `seg`, and
    /// what `visit` reports for it beneath it.
    pub fn slot<Seg, S>(
        &mut self,
        seg: Seg,
        slot: &Slot<S>,
        visit: impl FnOnce(&S, &mut Instances<'_>),
    ) where
        Seg: Eq + Hash + Send + Sync + 'static,
    {
        if let Some((value, id)) = slot.occupancy() {
            self.occupy(StructuralKey::new(seg), id, |out| visit(value, out));
        }
    }

    /// Reports one occupancy under `segment`, then what `visit` reports
    /// beneath it.
    fn occupy(
        &mut self,
        segment: StructuralKey,
        id: InstanceId,
        visit: impl FnOnce(&mut Instances<'_>),
    ) {
        let path = self.prefix.child(segment);
        self.report.push((path.clone(), id));
        visit(&mut Instances {
            prefix: path,
            report: &mut *self.report,
        });
    }
}

/// Reads the report `reducer` makes of `state`.
pub fn report<R: Reducer>(reducer: &R, state: &R::State) -> Report {
    let mut report = Report::new();
    reducer.instances(
        state,
        &mut Instances {
            prefix: ScopePath::empty(),
            report: &mut report,
        },
    );
    report
}

/// The previous report, and the comparison against it — the one
/// reconciliation the kernel and the store share (INV-RC3, RFC 0008 INV-T3).
///
/// Its constructor and [`update`](Self::update) are the whole seam: it is
/// built from the first report, and every update a kernel or a store drives
/// runs through `update`, which calls `reduce` and then reads the report
/// itself. So a caller that goes through it can neither begin without a
/// first report, reduce without reconciling, nor compare against a stale
/// report.
pub struct LiveInstances {
    previous: Report,
}

impl LiveInstances {
    /// Reads the first report, from the initial state; it tears nothing
    /// down.
    pub fn new<R: Reducer>(reducer: &R, state: &R::State) -> Self {
        Self {
            previous: report(reducer, state),
        }
    }

    /// Runs one update: calls `reduce`, reads the report of the state it
    /// left, and merges into the command `reduce` returned one teardown of
    /// each path whose pair the previous report holds and this one lacks,
    /// skipping a path under another such path, then keeps this report as
    /// the next comparison's baseline.
    ///
    /// The teardowns come in the previous report's order, so one script
    /// yields one sequence.
    ///
    /// It reads the whole report on every update, a cost RFC 0014 §13.6
    /// accepts until it is measured.
    pub fn update<R: Reducer>(
        &mut self,
        reducer: &R,
        state: &mut R::State,
        message: R::Message,
    ) -> Command<R::Message> {
        let command = reducer.reduce(state, message);
        let report = report(reducer, state);
        let outermost = disappeared_outermost(&self.previous, &report);
        self.previous = report;
        command.with_reconciled_teardowns(outermost)
    }
}

/// Each path under which `previous` holds a pair `current` lacks, once, at
/// the position of the first such pair, without the paths another of them is
/// a proper prefix of. The sets are consulted for membership only, never
/// iterated, so the order is the report's.
fn disappeared_outermost(previous: &Report, current: &Report) -> Vec<ScopePath> {
    let present: HashSet<&(ScopePath, InstanceId)> = current.iter().collect();
    let mut gone: HashSet<&[StructuralKey]> = HashSet::new();
    let mut disappeared: Vec<&ScopePath> = Vec::new();
    for pair in previous {
        if !present.contains(pair) && gone.insert(pair.0.segments()) {
            disappeared.push(&pair.0);
        }
    }
    disappeared
        .into_iter()
        .filter(|path| {
            let segments = path.segments();
            !(1..segments.len()).any(|len| gone.contains(&segments[..len]))
        })
        .cloned()
        .collect()
}
