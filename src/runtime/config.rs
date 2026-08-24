//! Construction-time configuration for the runtime entry points.
//!
//! [`RuntimeConfig`] carries the opt-in delivery controls (RFC 0006 as
//! RFC 0014 §9 rows 2 and 4 supersede it). With both unset — the state
//! [`RuntimeConfig::new`] produces — the configuration reproduces the
//! unbounded delivery mode exactly (RFC 0006 INV-L6), so it is equivalent to
//! [`Runtime::new`](crate::Runtime::new).

use std::num::NonZeroUsize;

/// Construction-time configuration for the runtime: the opt-in delivery
/// controls (RFC 0006).
///
/// It is the runtime's construction-time configuration, not a load-control
/// namespace: future runtime knobs accrete here rather than growing
/// [`Runtime`](crate::Runtime)'s constructor signature.
///
/// With both controls unset, the configuration reproduces the unbounded
/// delivery mode exactly (RFC 0006 INV-L6): the data lane is unbounded and
/// senders never wait. This is the mode
/// [`Runtime::new`](crate::Runtime::new) uses.
///
/// # Construction
///
/// [`RuntimeConfig::new`] is the sole constructor and leaves both controls
/// unset. The consuming setters
/// ([`data_lane_capacity`](Self::data_lane_capacity),
/// [`batch_max_messages`](Self::batch_max_messages)) opt into bounded
/// delivery and follow the crate's combinator convention (like
/// [`Command::timeout`](crate::Command::timeout)); each consumes the
/// configuration and returns the modified value. `RuntimeConfig` is [`Clone`]
/// but not [`Copy`], so a configuration reused after a setter call is cloned
/// explicitly.
///
/// # Examples
///
/// ```
/// # use std::num::NonZeroUsize;
/// # use tears::RuntimeConfig;
/// // Default (unbounded) delivery, equivalent to `Runtime::new`.
/// let config = RuntimeConfig::new();
///
/// // Opt into bounded delivery with the documented starting capacity.
/// let bounded =
///     RuntimeConfig::new().data_lane_capacity(NonZeroUsize::new(1024).expect("non-zero"));
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeConfig {
    /// Capacity of the single data lane every producer shares; `None` keeps
    /// it unbounded (RFC 0006 §4.1, RFC 0014 §3.1).
    pub(crate) data_lane_capacity: Option<NonZeroUsize>,
    /// Count cap for one input batch; `None` means the kernel's own default
    /// cap, which is finite either way (RFC 0014 §3.5).
    pub(crate) batch_max_messages: Option<NonZeroUsize>,
}

impl RuntimeConfig {
    /// Creates a configuration with both delivery controls unset.
    ///
    /// The unset controls reproduce the unbounded delivery mode (RFC 0006
    /// INV-L6): this configuration is equivalent to
    /// [`Runtime::new`](crate::Runtime::new).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            data_lane_capacity: None,
            batch_max_messages: None,
        }
    }

    /// Bounds the data lane to `capacity` messages, opting into bounded
    /// delivery (RFC 0006 §4.1).
    ///
    /// # Sizing — the latency/burst trade
    ///
    /// A bounded lane's capacity buys burst absorption and costs queueing
    /// latency: once the queue is full, a newly accepted message waits behind
    /// at most one full queue before reaching
    /// [`update`](crate::Application::update), so its wait is roughly
    /// `capacity × per-message drain cost`, where the drain cost is the
    /// application's own observed average loop service time (RFC 0006 §2). The
    /// product is an estimate on the measured workload, not a worst-case bound;
    /// an application that needs a hard bound derives it from its own measured
    /// tail service time.
    ///
    /// The **documented starting value is `1024`** — a measurement-informed
    /// margin choice (RFC 0007 §3.1), not a value the measurements pin
    /// uniquely; `512` or `2048` are defensible on the same data. Applications
    /// with larger expected bursts scale up by the same rule; the cost of
    /// undersizing is producer wait, never message loss (RFC 0006 INV-L2).
    ///
    /// **Every producer shares this lane.** Keyed command output, unkeyed
    /// command output and subscription output all travel it, so one key's
    /// backlog delays admission into it for every other producer — the
    /// per-command isolation the superseded topology's private channels gave
    /// is not preserved (RFC 0014 §9 row 2). A producer-originated quit is
    /// the one output that does not travel here: it takes the control lane,
    /// which is never bounded (RFC 0014 §3.3).
    ///
    /// One anti-pattern deserves naming: an application that spawns a command
    /// per processed message can, under bounded-mode overload, convert message
    /// backlog into *blocked-producer* backlog — blocked command tasks each
    /// holding one in-flight message — which no lane capacity bounds. The
    /// producer gauges (`tears::runtime::load`, RFC 0006 §4.4) make that
    /// pattern visible; restructure the effect flow rather than resizing the
    /// lane (RFC 0006 §4.5).
    #[must_use = "data_lane_capacity consumes the config and returns the modified value"]
    pub const fn data_lane_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.data_lane_capacity = Some(capacity);
        self
    }

    /// Caps the number of inputs one pass's batch may pull (RFC 0014 §3.5).
    ///
    /// The counted unit is a *pulled input*: every input the batch takes counts
    /// toward the cap — including the one that opened the batch and inputs that
    /// do not invoke [`update`](crate::Application::update) — so `Some(n)` lets
    /// the batch drain at most `n - 1` inputs after the first. A batch ends at
    /// the cap, or earlier when no input is ready.
    ///
    /// **Unset means the kernel's own default cap, not an uncapped batch.**
    /// The superseded 100µs time window is gone with the wall-clock reads it
    /// needed (RFC 0014 §9 row 4), and a pass's batch is finite by
    /// construction under every configuration; this control only replaces the
    /// default count with a smaller or larger one. Leaving it unset remains
    /// the recommendation — it is a diagnostic knob (RFC 0007 §3.1).
    #[must_use = "batch_max_messages consumes the config and returns the modified value"]
    pub const fn batch_max_messages(mut self, max: NonZeroUsize) -> Self {
        self.batch_max_messages = Some(max);
        self
    }

    /// The two construction-time controls, as
    /// `(data lane capacity, input batch count cap)`.
    pub(crate) const fn kernel_controls(&self) -> (Option<NonZeroUsize>, Option<NonZeroUsize>) {
        (self.data_lane_capacity, self.batch_max_messages)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::*;

    fn cap(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("capacity must be non-zero")
    }

    // INV-C2: `new` leaves both delivery controls unset.
    #[test]
    fn new_leaves_controls_unset() {
        let config = RuntimeConfig::new();

        assert_eq!(config.data_lane_capacity, None);
        assert_eq!(config.batch_max_messages, None);
    }

    // INV-C2: each setter sets exactly its own field and no other.
    #[test]
    fn data_lane_capacity_sets_only_its_field() {
        let config = RuntimeConfig::new().data_lane_capacity(cap(1024));

        assert_eq!(config.data_lane_capacity, Some(cap(1024)));
        assert_eq!(config.batch_max_messages, None);
    }

    #[test]
    fn batch_max_messages_sets_only_its_field() {
        let config = RuntimeConfig::new().batch_max_messages(cap(8));

        assert_eq!(config.batch_max_messages, Some(cap(8)));
        assert_eq!(config.data_lane_capacity, None);
    }

    // INV-C2/INV-C6: the setters chain, each independently, and every setter
    // consumes the config and returns the modified value (`#[must_use]`).
    #[test]
    fn setters_chain_independently() {
        let config = RuntimeConfig::new()
            .data_lane_capacity(cap(1024))
            .batch_max_messages(cap(4));

        assert_eq!(config.data_lane_capacity, Some(cap(1024)));
        assert_eq!(config.batch_max_messages, Some(cap(4)));
    }

    // INV-C6: a setter called on a clone — the one discard shape that survives
    // the move (the consuming-call misuse guard the `#[must_use]` messages
    // warn about) — modifies only the clone and leaves the original untouched.
    #[test]
    fn setter_on_a_clone_modifies_only_the_clone() {
        let config = RuntimeConfig::new();
        let modified = config.clone().data_lane_capacity(cap(1024));

        assert_eq!(modified.data_lane_capacity, Some(cap(1024)));
        assert_eq!(config.data_lane_capacity, None);
    }

    // The two controls the kernel reads, in the order it reads them.
    #[test]
    fn kernel_controls_pairs_the_lane_capacity_with_the_batch_cap() {
        let config = RuntimeConfig::new()
            .data_lane_capacity(cap(64))
            .batch_max_messages(cap(4));

        assert_eq!(config.kernel_controls(), (Some(cap(64)), Some(cap(4))));
    }
}
