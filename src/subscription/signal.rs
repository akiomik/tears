//! Signal subscription for handling OS signals.
//!
//! This module provides subscription sources for handling operating system signals
//! such as SIGINT, SIGTERM, etc. The implementation is platform-specific, with
//! different APIs for Unix and Windows systems.

#[cfg(unix)]
mod unix_signal {
    use std::{
        hash::{DefaultHasher, Hash, Hasher},
        io,
    };

    use futures::stream::BoxStream;
    use futures::{StreamExt as _, stream};
    use tokio::signal::unix::{Signal as TokioSignal, SignalKind, signal};

    use crate::subscription::{SubscriptionId, SubscriptionSource};

    /// A subscription source for Unix signals.
    ///
    /// This provides a stream of signal events for the specified signal kind.
    /// Each time the signal is received, the subscription emits a unit value `()`.
    ///
    /// # Platform
    ///
    /// This is only available on Unix platforms (Linux, macOS, BSD, etc.).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use tears::subscription::{Subscription, signal::Signal};
    /// use tokio::signal::unix::SignalKind;
    ///
    /// enum Message {
    ///     Interrupt,
    ///     Terminate,
    ///     SignalError(std::io::Error),
    /// }
    ///
    /// // Create a subscription for SIGINT (Ctrl+C)
    /// let sigint = Subscription::new(Signal::new(SignalKind::interrupt()))
    ///     .map(|result| match result {
    ///         Ok(()) => Message::Interrupt,
    ///         Err(e) => Message::SignalError(e),
    ///     });
    ///
    /// // Create a subscription for SIGTERM
    /// let sigterm = Subscription::new(Signal::new(SignalKind::terminate()))
    ///     .map(|result| match result {
    ///         Ok(()) => Message::Terminate,
    ///         Err(e) => Message::SignalError(e),
    ///     });
    /// ```
    ///
    /// # Error Handling
    ///
    /// This subscription yields `Result<(), io::Error>` values. The error case occurs
    /// when the signal handler cannot be installed (typically only at subscription
    /// creation time). Once the signal handler is successfully installed, it will
    /// yield `Ok(())` each time the signal is received.
    ///
    /// Common error scenarios:
    /// - **Signal handler installation failure**: Usually due to system limitations
    ///   or permission issues
    ///
    /// # Available Signals
    ///
    /// The `SignalKind` type from tokio supports many Unix signals including:
    /// - `SignalKind::interrupt()` - SIGINT (Ctrl+C)
    /// - `SignalKind::terminate()` - SIGTERM (termination request)
    /// - `SignalKind::hangup()` - SIGHUP (hangup)
    /// - `SignalKind::quit()` - SIGQUIT (quit with core dump)
    /// - `SignalKind::user_defined1()` - SIGUSR1 (user-defined signal 1)
    /// - `SignalKind::user_defined2()` - SIGUSR2 (user-defined signal 2)
    ///
    /// And many more. See the [`tokio::signal::unix::SignalKind`] documentation
    /// for the complete list.
    ///
    /// # Note
    ///
    /// Multiple subscriptions for the same signal kind are allowed. Each subscription
    /// will independently receive the signal.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct Signal {
        kind: SignalKind,
    }

    impl Signal {
        /// Create a new signal subscription for the specified signal kind.
        ///
        /// # Arguments
        ///
        /// * `kind` - The kind of signal to subscribe to
        ///
        /// # Example
        ///
        /// ```rust
        /// use tears::subscription::signal::Signal;
        /// use tokio::signal::unix::SignalKind;
        ///
        /// // Subscribe to SIGINT
        /// let sigint = Signal::new(SignalKind::interrupt());
        ///
        /// // Subscribe to SIGTERM
        /// let sigterm = Signal::new(SignalKind::terminate());
        /// ```
        #[must_use]
        pub const fn new(kind: SignalKind) -> Self {
            Self { kind }
        }
    }

    impl SubscriptionSource for Signal {
        type Output = Result<(), io::Error>;

        fn stream(&self) -> BoxStream<'static, Self::Output> {
            let kind = self.kind;

            // Create a stream that yields () each time the signal is received
            // NOTE: signal() returns Result<Signal, io::Error>
            // If it fails, try_unfold yields a single error and ends the stream
            // If it succeeds, it yields Ok(()) for each signal received
            stream::try_unfold(None, move |state: Option<TokioSignal>| async move {
                let mut sig = match state {
                    None => {
                        // First call: initialize the signal handler
                        signal(kind)?
                    }
                    Some(sig) => sig,
                };

                // Wait for signals
                match sig.recv().await {
                    Some(()) => Ok(Some(((), Some(sig)))),
                    None => Ok(None), // End the stream
                }
            })
            .boxed()
        }

        fn id(&self) -> SubscriptionId {
            let mut hasher = DefaultHasher::new();
            self.hash(&mut hasher);
            SubscriptionId::of::<Self>(hasher.finish())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_signal_new() {
            let sig = Signal::new(SignalKind::interrupt());
            assert_eq!(sig.kind, SignalKind::interrupt());
        }

        #[test]
        fn test_signal_id_consistency() {
            let sig1 = Signal::new(SignalKind::interrupt());
            let sig2 = Signal::new(SignalKind::interrupt());

            // Same signal kind should produce the same ID
            assert_eq!(sig1.id(), sig2.id());
        }

        #[test]
        fn test_signal_id_different_kinds() {
            let sig1 = Signal::new(SignalKind::interrupt());
            let sig2 = Signal::new(SignalKind::terminate());

            // Different signal kinds should produce different IDs
            assert_ne!(sig1.id(), sig2.id());
        }
    }
}

#[cfg(unix)]
pub use unix_signal::Signal;

#[cfg(windows)]
mod windows_signal {
    use std::{
        hash::{DefaultHasher, Hash, Hasher},
        io,
    };

    use futures::StreamExt as _;
    use futures::stream::{self, BoxStream};
    use tokio::signal::windows::{
        CtrlBreak as TokioCtrlBreak, CtrlC as TokioCtrlC, ctrl_break, ctrl_c,
    };

    use crate::subscription::{SubscriptionId, SubscriptionSource};

    /// A subscription source for Windows Ctrl+C events.
    ///
    /// This provides a stream of Ctrl+C events. Each time Ctrl+C is pressed,
    /// the subscription emits a unit value `()`.
    ///
    /// # Platform
    ///
    /// This is only available on Windows platforms.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use tears::subscription::{Subscription, signal::CtrlC};
    ///
    /// enum Message {
    ///     CtrlC,
    ///     SignalError(std::io::Error),
    /// }
    ///
    /// // Create a subscription for Ctrl+C
    /// let ctrl_c = Subscription::new(CtrlC::new())
    ///     .map(|result| match result {
    ///         Ok(()) => Message::CtrlC,
    ///         Err(e) => Message::SignalError(e),
    ///     });
    /// ```
    ///
    /// # Error Handling
    ///
    /// This subscription yields `Result<(), io::Error>` values. The error case occurs
    /// when the signal handler cannot be installed (typically only at subscription
    /// creation time). Once the signal handler is successfully installed, it will
    /// yield `Ok(())` each time Ctrl+C is pressed.
    ///
    /// # Note
    ///
    /// This is a singleton subscription - all instances are considered identical
    /// and only one Ctrl+C handler will be active at a time.
    #[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
    pub struct CtrlC;

    impl CtrlC {
        /// Create a new Ctrl+C subscription.
        ///
        /// # Example
        ///
        /// ```rust
        /// use tears::subscription::signal::CtrlC;
        ///
        /// let ctrl_c = CtrlC::new();
        /// ```
        #[must_use]
        pub const fn new() -> Self {
            Self
        }
    }

    impl SubscriptionSource for CtrlC {
        type Output = Result<(), io::Error>;

        fn stream(&self) -> BoxStream<'static, Self::Output> {
            // Create a stream that yields () each time Ctrl+C is pressed
            // NOTE: tokio::signal::windows::ctrl_c() returns Result<CtrlC, io::Error>
            // If it fails, try_unfold yields a single error and ends the stream
            // If it succeeds, it yields Ok(()) for each event received
            stream::try_unfold(None, move |state: Option<TokioCtrlC>| async move {
                let mut sig = match state {
                    None => {
                        // First call: initialize the signal handler
                        ctrl_c()?
                    }
                    Some(sig) => sig,
                };

                // Wait for signals
                match sig.recv().await {
                    Some(()) => Ok(Some(((), Some(sig)))),
                    None => Ok(None), // End the stream
                }
            })
            .boxed()
        }

        fn id(&self) -> SubscriptionId {
            let mut hasher = DefaultHasher::new();
            self.hash(&mut hasher);
            SubscriptionId::of::<Self>(hasher.finish())
        }
    }

    /// A subscription source for Windows Ctrl+Break events.
    ///
    /// This provides a stream of Ctrl+Break events. Each time Ctrl+Break is pressed,
    /// the subscription emits a unit value `()`.
    ///
    /// # Platform
    ///
    /// This is only available on Windows platforms.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use tears::subscription::{Subscription, signal::CtrlBreak};
    ///
    /// enum Message {
    ///     CtrlBreak,
    ///     SignalError(std::io::Error),
    /// }
    ///
    /// // Create a subscription for Ctrl+Break
    /// let ctrl_break = Subscription::new(CtrlBreak::new())
    ///     .map(|result| match result {
    ///         Ok(()) => Message::CtrlBreak,
    ///         Err(e) => Message::SignalError(e),
    ///     });
    /// ```
    ///
    /// # Error Handling
    ///
    /// This subscription yields `Result<(), io::Error>` values. The error case occurs
    /// when the signal handler cannot be installed (typically only at subscription
    /// creation time). Once the signal handler is successfully installed, it will
    /// yield `Ok(())` each time Ctrl+Break is pressed.
    ///
    /// # Note
    ///
    /// This is a singleton subscription - all instances are considered identical
    /// and only one Ctrl+Break handler will be active at a time.
    #[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
    pub struct CtrlBreak;

    impl CtrlBreak {
        /// Create a new Ctrl+Break subscription.
        ///
        /// # Example
        ///
        /// ```rust
        /// use tears::subscription::signal::CtrlBreak;
        ///
        /// let ctrl_break = CtrlBreak::new();
        /// ```
        #[must_use]
        pub const fn new() -> Self {
            Self
        }
    }

    impl SubscriptionSource for CtrlBreak {
        type Output = Result<(), io::Error>;

        fn stream(&self) -> BoxStream<'static, Self::Output> {
            // Create a stream that yields () each time Ctrl+Break is pressed
            // NOTE: tokio::signal::windows::ctrl_break() returns Result<CtrlBreak, io::Error>
            // If it fails, try_unfold yields a single error and ends the stream
            // If it succeeds, it yields Ok(()) for each event received
            stream::try_unfold(None, move |state: Option<TokioCtrlBreak>| async move {
                let mut sig = match state {
                    None => {
                        // First call: initialize the signal handler
                        ctrl_break()?
                    }
                    Some(sig) => sig,
                };

                // Wait for signals
                match sig.recv().await {
                    Some(()) => Ok(Some(((), Some(sig)))),
                    None => Ok(None), // End the stream
                }
            })
            .boxed()
        }

        fn id(&self) -> SubscriptionId {
            let mut hasher = DefaultHasher::new();
            self.hash(&mut hasher);
            SubscriptionId::of::<Self>(hasher.finish())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_ctrl_c_id_consistency() {
            let ctrl_c1 = CtrlC::new();
            let ctrl_c2 = CtrlC::new();

            // Same subscription should have the same ID
            assert_eq!(ctrl_c1.id(), ctrl_c2.id());
        }

        #[test]
        fn test_ctrl_break_id_consistency() {
            let ctrl_break1 = CtrlBreak::new();
            let ctrl_break2 = CtrlBreak::new();

            // Same subscription should have the same ID
            assert_eq!(ctrl_break1.id(), ctrl_break2.id());
        }

        #[test]
        fn test_ctrl_c_and_ctrl_break_different_ids() {
            let ctrl_c = CtrlC::new();
            let ctrl_break = CtrlBreak::new();

            // Different signal types should have different IDs
            assert_ne!(ctrl_c.id(), ctrl_break.id());
        }
    }
}

#[cfg(windows)]
pub use windows_signal::{CtrlBreak, CtrlC};
