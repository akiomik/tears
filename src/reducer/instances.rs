//! The live-instance report and the reconciliation that reads it (RFC 0014
//! §2.5, INV-RC3/INV-RC3a).
//!
//! A reducer reports the occupancies its state holds through
//! [`Reducer::instances`](super::Reducer::instances) as (qualified path,
//! identity) pairs. After every update, the kernel and the store compare the
//! report with the one before and merge one teardown per path that
//! disappeared into the command they dispatch for that update.

use std::collections::HashSet;

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
/// A reducer reports where it places work: each row or occupant it
/// qualifies commands under, through [`keyed`](Self::keyed) or
/// [`slot`](Self::slot), with the report of the child it reduces there
/// beneath it, and under [`scoped`](Self::scoped) the report of a child it
/// reduces under a fixed segment (RFC 0014 INV-RC3a). One that places no
/// work under a segment reports nothing. The combinators do all of this for
/// you.
pub struct Instances<'a> {
    prefix: ScopePath,
    report: &'a mut Report,
}

impl Instances<'_> {
    /// Reports what `visit` reports, one boundary down, under `seg`.
    ///
    /// This is what a [`scope`](super::ReducerExt::scope) boundary does: it
    /// adds a segment and no occupancy of its own.
    pub fn scoped<Seg: ScopeValue>(&mut self, seg: Seg, visit: impl FnOnce(&mut Instances<'_>)) {
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
            let path = self.prefix.child(StructuralKey::new(key.clone()));
            self.report.push((path.clone(), id));
            let mut child = Instances {
                prefix: path,
                report: &mut *self.report,
            };
            visit(value, &mut child);
        }
    }

    /// Reports the occupant of `slot`, if there is one, under `seg`, and
    /// what `visit` reports for it beneath it.
    pub fn slot<Seg: ScopeValue, S>(
        &mut self,
        seg: Seg,
        slot: &Slot<S>,
        visit: impl FnOnce(&S, &mut Instances<'_>),
    ) {
        if let Some((value, id)) = slot.occupancy() {
            let path = self.prefix.child(StructuralKey::new(seg));
            self.report.push((path.clone(), id));
            let mut child = Instances {
                prefix: path,
                report: &mut *self.report,
            };
            visit(value, &mut child);
        }
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
/// Its two methods are the whole seam: every update a kernel or a store
/// drives reaches its dispatch or intake through [`reconcile`](Self::reconcile),
/// which reads the report itself, so no caller can compare against a stale
/// report or forget to read one.
#[derive(Default)]
pub struct LiveInstances {
    previous: Report,
}

impl LiveInstances {
    /// Reads the first report, from the initial state; it tears nothing
    /// down.
    pub fn seed<R: Reducer>(&mut self, reducer: &R, state: &R::State) {
        self.previous = report(reducer, state);
    }

    /// Reads the report of the state an update left and merges into
    /// `command` one teardown of each path whose pair the previous report
    /// holds and this one lacks, skipping a path under another such path,
    /// then keeps this report as the next comparison's baseline.
    ///
    /// The teardowns come in the order the previous report first named each
    /// path, so one script yields one sequence; the sets below are consulted
    /// for membership only, never iterated.
    pub fn reconcile<R: Reducer>(
        &mut self,
        reducer: &R,
        state: &R::State,
        command: Command<R::Message>,
    ) -> Command<R::Message> {
        let report = report(reducer, state);
        let current: HashSet<&(ScopePath, InstanceId)> = report.iter().collect();
        let mut seen: HashSet<&ScopePath> = HashSet::new();
        let mut disappeared: Vec<&ScopePath> = Vec::new();
        for pair in &self.previous {
            if !current.contains(pair) && seen.insert(&pair.0) {
                disappeared.push(&pair.0);
            }
        }
        let outermost: Vec<ScopePath> = disappeared
            .iter()
            .filter(|path| !path.proper_prefixes().any(|prefix| seen.contains(&prefix)))
            .map(|path| (*path).clone())
            .collect();
        let command = command.with_reconciled_teardowns(outermost);
        self.previous = report;
        command
    }
}
