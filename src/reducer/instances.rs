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
pub(crate) type Report = Vec<(ScopePath, InstanceId)>;

/// What a reducer reports its occupancies through.
///
/// It is append-only and opaque: an implementation can descend under a
/// segment and report the rows of a [`Keyed`] or the occupant of a [`Slot`],
/// and nothing else — it cannot read, remove, or invent a report entry.
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
pub(crate) fn report<R: Reducer>(reducer: &R, state: &R::State) -> Report {
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
#[derive(Default)]
pub(crate) struct LiveInstances {
    previous: Report,
}

impl LiveInstances {
    /// Takes the first report, which tears nothing down.
    pub(crate) fn seed(&mut self, report: Report) {
        self.previous = report;
    }

    /// Merges into `command` one teardown of each path whose pair the
    /// previous report holds and `report` lacks, skipping a path another
    /// such path is a proper prefix of, and keeps `report` as the next
    /// comparison's baseline.
    ///
    /// The teardowns come in the order the previous report first named each
    /// path, so one script yields one sequence; the set below is consulted
    /// for membership only, never iterated.
    pub(crate) fn reconcile<Msg: Send + 'static>(
        &mut self,
        report: Report,
        command: Command<Msg>,
    ) -> Command<Msg> {
        let current: HashSet<&(ScopePath, InstanceId)> = report.iter().collect();
        let mut disappeared: Vec<&ScopePath> = Vec::new();
        for pair in &self.previous {
            if !current.contains(pair) && !disappeared.contains(&&pair.0) {
                disappeared.push(&pair.0);
            }
        }
        let outermost: Vec<ScopePath> = disappeared
            .iter()
            .filter(|path| {
                !disappeared
                    .iter()
                    .any(|other| other != *path && path.starts_with(other))
            })
            .map(|path| (*path).clone())
            .collect();
        drop(current);
        self.previous = report;
        if outermost.is_empty() {
            command
        } else {
            command.merging_teardowns(Command::reconciled_teardowns(outermost))
        }
    }
}
