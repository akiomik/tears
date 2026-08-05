//! Construction-time configuration for the [`Runtime`](crate::Runtime).
//!
//! [`RuntimeConfig`] carries the frame rate together with the opt-in
//! load-control knobs (RFC 0006). With the load controls unset — the state
//! [`RuntimeConfig::new`] produces — the configuration reproduces the
//! unbounded delivery mode exactly (RFC 0006 INV-L6), so it is equivalent to
//! [`Runtime::new`](crate::Runtime::new).

use std::num::NonZeroUsize;

use super::frame_rate::FrameRate;

/// Construction-time configuration for the runtime: the frame rate and the
/// opt-in load controls (RFC 0006).
///
/// `RuntimeConfig` is the runtime's construction-time configuration, not a
/// load-control namespace — the frame rate is a runtime setting like any
/// other, and future runtime knobs accrete here rather than growing
/// [`Runtime`](crate::Runtime)'s constructor signatures.
///
/// With the load controls unset, the configuration reproduces the unbounded
/// delivery mode exactly (RFC 0006 INV-L6): the shared and keyed channels are
/// unbounded and senders never wait. This is the mode
/// [`Runtime::new`](crate::Runtime::new) uses.
///
/// # Construction
///
/// [`RuntimeConfig::new`] is the sole constructor — it takes the one setting
/// that has no meaningful default (the frame rate) and leaves the three load
/// controls unset. The consuming setters
/// ([`app_channel_capacity`](Self::app_channel_capacity),
/// [`keyed_channel_capacity`](Self::keyed_channel_capacity),
/// [`batch_max_messages`](Self::batch_max_messages)) opt into bounded delivery
/// and follow the crate's combinator convention (like
/// [`Command::timeout`](crate::Command::timeout)); each returns a modified
/// copy and does not mutate in place.
///
/// There is deliberately no [`Default`] impl: the crate has no default frame
/// rate, so a value would have to be invented inside the derive.
///
/// # Examples
///
/// ```
/// # use std::num::{NonZeroU32, NonZeroUsize};
/// # use tears::{FrameRate, RuntimeConfig};
/// let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero"))
///     .expect("valid frame rate");
///
/// // Default (unbounded) delivery, equivalent to `Runtime::new`.
/// let config = RuntimeConfig::new(frame_rate);
///
/// // Opt into bounded delivery with the documented starting capacities.
/// let bounded = RuntimeConfig::new(frame_rate)
///     .app_channel_capacity(NonZeroUsize::new(1024).expect("non-zero"))
///     .keyed_channel_capacity(NonZeroUsize::new(16).expect("non-zero"));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    /// Target frame rate for the runtime's frame scheduler.
    pub(crate) frame_rate: FrameRate,
    /// Capacity of the shared application-message channel; `None` keeps it
    /// unbounded (RFC 0006 §4.1).
    pub(crate) app_channel_capacity: Option<NonZeroUsize>,
    /// Capacity of each keyed command's private channel; `None` keeps it
    /// unbounded (RFC 0006 §4.1).
    pub(crate) keyed_channel_capacity: Option<NonZeroUsize>,
    /// Count cap for one micro-batch window; `None` keeps the time-capped-only
    /// loop (RFC 0006 §4.1, INV-L12).
    pub(crate) batch_max_messages: Option<NonZeroUsize>,
}

impl RuntimeConfig {
    /// Creates a configuration with the given frame rate and the three load
    /// controls unset.
    ///
    /// The unset load controls reproduce the unbounded delivery mode (RFC 0006
    /// INV-L6): this configuration is equivalent to
    /// [`Runtime::new`](crate::Runtime::new) with the same frame rate.
    #[must_use]
    pub const fn new(frame_rate: FrameRate) -> Self {
        Self {
            frame_rate,
            app_channel_capacity: None,
            keyed_channel_capacity: None,
            batch_max_messages: None,
        }
    }

    /// Bounds the shared application-message channel to `capacity` messages,
    /// opting into bounded delivery for it (RFC 0006 §4.1).
    ///
    /// # Sizing — the latency/burst trade
    ///
    /// A bounded shared channel's capacity buys burst absorption and costs
    /// queueing latency: once the queue is full, a newly accepted message
    /// waits behind at most one full queue before reaching
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
    /// One anti-pattern deserves naming: an application that spawns a command
    /// per processed message can, under bounded-mode overload, convert message
    /// backlog into *blocked-producer* backlog — blocked command tasks each
    /// holding one in-flight message — which no channel capacity bounds. The
    /// producer gauges (`tears::runtime::load`, RFC 0006 §4.4) make that
    /// pattern visible; restructure the effect flow rather than resizing the
    /// channel (RFC 0006 §4.5).
    #[must_use = "app_channel_capacity returns a modified config and does not mutate in place"]
    pub const fn app_channel_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.app_channel_capacity = Some(capacity);
        self
    }

    /// Bounds each keyed command's private channel to `capacity` messages,
    /// opting into bounded delivery for keyed output (RFC 0006 §4.1).
    ///
    /// # Sizing — burst absorption and memory, not a latency guarantee
    ///
    /// The `capacity × drain cost` estimate does **not** transfer to the keyed
    /// channel: while any shared input stays ready the keyed channel is not
    /// drained at all (shared-first pull, RFC 0006 §4.7), so keyed-delivery
    /// latency stays unbounded independent of the configured capacity. A larger
    /// keyed capacity bounds no delivery latency and restores no keyed
    /// liveness; what it buys in a finite execution is reduced producer-side
    /// admission wait — a burst of up to the channel's free capacity completes
    /// without the command blocking — at a memory cost (the per-command share
    /// of the `m × capacity` buffer total, RFC 0006 R1). Size it for that
    /// absorption-versus-memory trade, never for a delivery-latency guarantee.
    ///
    /// The **documented starting value is `16`** — a margin choice, not
    /// measurement-derived (RFC 0007 §3.1).
    ///
    /// Keying a command buys cancellation at the cost of deferral behind ready
    /// shared inputs; liveness-critical output belongs in an unkeyed command,
    /// and keyed liveness under load comes from pacing hot sources, not from
    /// bounded mode (RFC 0006 §4.7).
    ///
    /// The same trade decides how to quit: use unkeyed
    /// [`Command::quit`](crate::Command::quit) for a prompt, unconditional
    /// quit with backlog-independent delivery; putting `.cancellable(id)` on a
    /// quit buys suppression — a later cancel or supersede can still stop it —
    /// at the cost of waiting behind pending inputs under load
    /// (RFC 0006 §4.6).
    #[must_use = "keyed_channel_capacity returns a modified config and does not mutate in place"]
    pub const fn keyed_channel_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.keyed_channel_capacity = Some(capacity);
        self
    }

    /// Caps the number of inputs one micro-batch window may pull, complementing
    /// the 100µs time cap (RFC 0006 §4.1, INV-L12).
    ///
    /// The counted unit is a *pulled input*: every input the batch takes counts
    /// toward the cap — including the one that opened the batch and inputs that
    /// do not invoke [`update`](crate::Application::update) — so `Some(n)` lets
    /// the batch drain at most `n - 1` inputs after the first. A batch ends at
    /// whichever cap is reached first (count or the 100µs window), or earlier
    /// when no input is ready or a quit is pulled.
    ///
    /// The **documented recommendation is to leave this unset**: the 100µs time
    /// cap alone holds the frame branch stable under overload (RFC 0006 F5), so
    /// the count cap is a diagnostic knob, not a default (RFC 0007 §3.1).
    #[must_use = "batch_max_messages returns a modified config and does not mutate in place"]
    pub const fn batch_max_messages(mut self, max: NonZeroUsize) -> Self {
        self.batch_max_messages = Some(max);
        self
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroUsize};

    use super::*;

    fn frame_rate(value: u32) -> FrameRate {
        FrameRate::new(NonZeroU32::new(value).expect("frame rate must be non-zero"))
            .expect("frame rate must be valid")
    }

    fn cap(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("capacity must be non-zero")
    }

    // INV-C2: `new` carries the given frame rate and leaves all three load
    // controls unset.
    #[test]
    fn new_carries_frame_rate_and_leaves_controls_unset() {
        let config = RuntimeConfig::new(frame_rate(60));

        assert_eq!(config.frame_rate, frame_rate(60));
        assert_eq!(config.app_channel_capacity, None);
        assert_eq!(config.keyed_channel_capacity, None);
        assert_eq!(config.batch_max_messages, None);
    }

    // INV-C2: each setter sets exactly its own field and no other.
    #[test]
    fn app_channel_capacity_sets_only_its_field() {
        let config = RuntimeConfig::new(frame_rate(60)).app_channel_capacity(cap(1024));

        assert_eq!(config.app_channel_capacity, Some(cap(1024)));
        assert_eq!(config.frame_rate, frame_rate(60));
        assert_eq!(config.keyed_channel_capacity, None);
        assert_eq!(config.batch_max_messages, None);
    }

    #[test]
    fn keyed_channel_capacity_sets_only_its_field() {
        let config = RuntimeConfig::new(frame_rate(60)).keyed_channel_capacity(cap(16));

        assert_eq!(config.keyed_channel_capacity, Some(cap(16)));
        assert_eq!(config.frame_rate, frame_rate(60));
        assert_eq!(config.app_channel_capacity, None);
        assert_eq!(config.batch_max_messages, None);
    }

    #[test]
    fn batch_max_messages_sets_only_its_field() {
        let config = RuntimeConfig::new(frame_rate(60)).batch_max_messages(cap(8));

        assert_eq!(config.batch_max_messages, Some(cap(8)));
        assert_eq!(config.frame_rate, frame_rate(60));
        assert_eq!(config.app_channel_capacity, None);
        assert_eq!(config.keyed_channel_capacity, None);
    }

    // INV-C2/INV-C6: the setters chain, each independently, and every setter
    // returns a modified copy (`#[must_use]`) rather than mutating in place.
    #[test]
    fn setters_chain_independently() {
        let config = RuntimeConfig::new(frame_rate(30))
            .app_channel_capacity(cap(1024))
            .keyed_channel_capacity(cap(16))
            .batch_max_messages(cap(4));

        assert_eq!(config.frame_rate, frame_rate(30));
        assert_eq!(config.app_channel_capacity, Some(cap(1024)));
        assert_eq!(config.keyed_channel_capacity, Some(cap(16)));
        assert_eq!(config.batch_max_messages, Some(cap(4)));
    }

    // INV-C3: a discarded setter call leaves the original value untouched
    // (the consuming-call misuse guard the `#[must_use]` messages warn about).
    #[test]
    fn discarded_setter_leaves_original_unmodified() {
        let config = RuntimeConfig::new(frame_rate(60));
        let _ = config.app_channel_capacity(cap(1024));

        assert_eq!(config.app_channel_capacity, None);
    }
}
