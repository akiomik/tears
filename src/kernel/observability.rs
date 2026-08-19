//! The kernel's side of the `tears::runtime::load` schema (RFC 0006 §4.4,
//! INV-L13), under RFC 0014 §9 row 9's vocabulary amendment.
//!
//! **Nothing here is a new schema.** Row 9 re-reads the existing event
//! vocabulary and leaves every field name where it is — deliberately the
//! opposite of row 2's decision for the configuration control, because a
//! renamed telemetry field breaks dashboards and log parsers silently, off
//! the compiler's path entirely. So this module emits the *same* events, on
//! the same target, at the same level, with the same fields, from the
//! kernel's own driving loop:
//!
//! | Event | Emitter | What the kernel's reading is |
//! | --- | --- | --- |
//! | batch | [`Kernel::report_batch`] | `pulled` = the input batch's dequeues, `updated` = those that reached `reduce`, `shared_pending` = the **data lane's** residual occupancy |
//! | capacity wait | `runtime::channel`'s bounded send | `channel` takes the single value `"data"`: every producer shares one lane, so a blocked send has only one thing to name (INV-RC15's behavioural neighbour) |
//! | producer gauges | `producer`'s guards and `registry`'s publish point | `unkeyed_commands` = anonymous runs, `keyed_commands` = keyed runs, `subscriptions` = subscription runs, `blocked` = producers awaiting lane capacity |
//!
//! The last two rows are already in place from the lane and the run
//! bookkeeping — the data lane is built through `channel_observed` under
//! [`Channel::Data`], and the gauge guards ride the producer harness — so
//! what this module adds is the batch event and the reading of its three
//! fields.
//!
//! **`shared_pending` is the one field whose meaning moved.** It named the
//! shared application-message channel's occupancy, and the shared channel is
//! gone: RFC 0014 §9 row 2 supersedes the two delivery classes with one FIFO
//! data lane. The successor quantity is that lane's residual — what the batch
//! left behind — which is the same operational question a consumer was
//! asking, on the object that now carries it.
//!
//! **Cleanup runs count in no gauge field.** They are runtime-owned runs, but
//! the four fields name producer kinds and a cleanup run is not one (RFC 0006
//! §5.2's successor note); the settle loop reads
//! [`Kernel::owned_task_count`] instead, which is not part of any schema.
//!
//! [`Channel::Data`]: crate::runtime::load::Channel::Data

use crate::reducer::Program;
use crate::runtime::channel::Receiver;
use crate::runtime::load;

use super::Kernel;

impl<P: Program> Kernel<P> {
    /// Emits the batch event for one completed input batch (RFC 0006 §4.4).
    ///
    /// Row 9 leaves the firing condition unchanged, and the two guards here
    /// are what keep it so. The old loop's batch *began* with an input, so a
    /// batch with nothing pulled had no existence to report, and a batch a
    /// quit cut short left through the loop's exit without emitting. The
    /// kernel runs stage 3 on every pass instead — including passes an exit
    /// or a control arrival began — so a pass that pulled nothing is a case
    /// the old shape could not produce, and emitting for it would widen the
    /// event's meaning under an unchanged name.
    ///
    /// The *rule* is therefore preserved; its **reach** narrows, and the
    /// narrowing is not this module's doing. The old loop exited early only
    /// for a quit that arrived as an **input** — a keyed command's — while
    /// an `update`-returned `Command::quit()` was an ordinary dispatch
    /// there, so its batch ran on and reported. RFC 0014 §3.3 applies that
    /// quit synchronously at its dispatch, so the batch carrying it now ends
    /// without an event, and a producer quit reaches the control drain a
    /// stage before the batch rather than mid-way through one. Both follow
    /// from the quit routes that row supersedes.
    pub(super) fn report_batch(&self, pulled: usize, updated: usize) {
        if pulled == 0 || self.terminating() {
            return;
        }
        load::batch(pulled, updated, self.data_residue());
    }

    /// The data lane's residual occupancy — `shared_pending`'s successor
    /// quantity (RFC 0014 §9 row 9).
    ///
    /// The park's buffer counts beside the lane. An envelope a park took off
    /// the lane is still undelivered input the batch left behind, and a
    /// reading that skipped it would report an empty lane while the very
    /// next pass delivers from the buffer.
    fn data_residue(&self) -> usize {
        self.data_buf.len() + self.data_rx.as_ref().map_or(0, Receiver::len)
    }
}
