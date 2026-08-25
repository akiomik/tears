//! Entry points for running TUI applications.
//!
//! Two of them, over one execution path. [`Runtime`] runs an
//! [`Application`]; [`ProgramRuntime`] runs any [`Program`], which is what a
//! stack of composition combinators closes into. Both build the same
//! [`Kernel`](crate::kernel::Kernel) over the same lanes and drive the same
//! pass loop — the facade adds a mapping adapter and nothing else, which is
//! the structural half of RFC 0014 INV-RC1.
//!
//! Construction is inert: neither entry point starts a task, opens a
//! terminal, or calls into the application until `run` is awaited (RFC 0011
//! INV-LC3). `run` consumes the runtime and returns when the loop has
//! terminated *and* settled — a quit of either route classifies as success,
//! a render failure as the backend's error (RFC 0011 INV-LC5).
//!
//! # Example
//!
//! ```rust,no_run
//! use color_eyre::eyre::Result;
//! use ratatui::Frame;
//! use tears::prelude::*;
//!
//! struct CounterApp {
//!     count: i32,
//! }
//!
//! enum Message {
//!     Increment,
//!     Quit,
//! }
//!
//! impl Application for CounterApp {
//!     type Message = Message;
//!     type Flags = ();
//!
//!     fn new(_flags: ()) -> (Self, Command<Message>) {
//!         (Self { count: 0 }, Command::none())
//!     }
//!
//!     fn update(&mut self, msg: Message) -> Command<Message> {
//!         match msg {
//!             Message::Increment => {
//!                 self.count += 1;
//!                 Command::none()
//!             }
//!             Message::Quit => Command::quit(),
//!         }
//!     }
//!
//!     fn view(&self, frame: &mut Frame<'_>) {
//!         // Render UI...
//!     }
//!
//!     fn subscriptions(&self) -> Vec<Subscription<Message>> {
//!         vec![]
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let runtime = Runtime::<CounterApp>::new(());
//!     let mut terminal = ratatui::init();
//!     // Restore the terminal if the application panics.
//!     tears::install_panic_hook();
//!     runtime.run(&mut terminal).await?;
//!     ratatui::restore();
//!     Ok(())
//! }
//! ```

use ratatui::Terminal;
use ratatui::prelude::Backend;

use crate::application::Application;
use crate::kernel::Kernel;
use crate::kernel::lane::GateMode;
use crate::reducer::adapter::AppProgram;
use crate::reducer::{Exit, Program};

// `pub` (not `pub(crate)`) so `subscription`'s forwarding task can name the
// data-lane `Sender` it feeds: `runtime` is already `pub(crate)`, so `pub`
// caps this submodule's effective reachability at the crate the same way,
// while avoiding the redundant-`pub(crate)` lint. The type carries no public
// API.
pub mod channel;
// `pub` for the same crate-capping reason: `runtime` is already
// `pub(crate)`, so `pub` only lets `lib.rs` re-export `RuntimeConfig` at the
// crate root; it does not widen effective reachability.
pub mod config;
// `pub` for the same crate-capping reason as `channel`: crate-internal
// load-observability types (`tears::runtime::load` schema, RFC 0006 §4.4)
// that `subscription` and the lane producers share.
pub mod load;

use config::RuntimeConfig;
use load::LoadObserver;

/// Runs a [`Program`] — the entry point for a composed reducer stack.
///
/// [`Runtime`] is this type with [`Application`]'s adapter already applied;
/// everything below the adapter is shared, so what is written here about
/// lanes, passes and termination holds verbatim there (RFC 0014 §2.3).
///
/// # Type Parameters
///
/// * `P` - the program to run, typically the result of
///   [`ReducerExt::into_program`](crate::reducer::ReducerExt::into_program)
#[must_use = "a runtime is inert until it is run; call .run(terminal) to execute the program"]
pub struct ProgramRuntime<P: Program> {
    program: P,
    flags: P::Flags,
    config: RuntimeConfig,
}

impl<P: Program> ProgramRuntime<P> {
    /// Creates a runtime for `program`, to be initialized with `flags`.
    ///
    /// The delivery controls take their defaults: an unbounded data lane and
    /// the kernel's own batch cap (RFC 0006 INV-L6's unbounded mode).
    /// [`with_config`](Self::with_config) sets them.
    ///
    /// Construction is inert — `flags` are held, not consumed, until
    /// [`run`](Self::run) initializes the program (RFC 0011 INV-LC3).
    pub const fn new(program: P, flags: P::Flags) -> Self {
        Self::with_config(program, flags, RuntimeConfig::new())
    }

    /// Creates a runtime for `program` with explicit delivery controls.
    ///
    /// `config` is moved rather than copied, so a setter's return value
    /// cannot be discarded and the original silently reused (RFC 0007 §2.1).
    pub const fn with_config(program: P, flags: P::Flags, config: RuntimeConfig) -> Self {
        Self {
            program,
            flags,
            config,
        }
    }

    /// Runs the program to termination, then settles.
    ///
    /// Returns once the loop has terminated *and* every runtime-owned task
    /// has been accounted for, under either classification: the quiescent
    /// postcondition is reached whether the run ended in a quit or in a
    /// render failure, and only the return value tells them apart (RFC 0011
    /// INV-LC5, RFC 0014 §6.1).
    ///
    /// # Errors
    ///
    /// Returns the backend's error when a render failed. The runtime has
    /// terminated and settled by then; the error is the classification, not
    /// an escape from the postconditions.
    pub async fn run<B: Backend>(self, terminal: &mut Terminal<B>) -> Result<Exit, B::Error> {
        let mut kernel = Kernel::new(
            self.program,
            self.flags,
            &self.config,
            GateMode::Immediate,
            LoadObserver::new(),
        );
        kernel.run(terminal).await.map(|_report| Exit::Quit)
    }
}

/// Runtime that runs an [`Application`].
///
/// The facade over [`ProgramRuntime`]: it wraps `App` in the adapter that
/// makes an application a program — the application value *is* the state,
/// `update` *is* `reduce` — and runs it on the same kernel a composed
/// program runs on. There is no `Application`-specific channel, branch or
/// phase anywhere below the adapter (RFC 0014 INV-RC1).
///
/// # Type Parameters
///
/// * `App` - The application type implementing [`Application`]
///
/// # Examples
///
/// See the [crate-level documentation](crate) for a complete example.
#[must_use = "a runtime is inert until it is run; call .run(terminal) to execute the application"]
pub struct Runtime<App: Application> {
    inner: ProgramRuntime<AppProgram<App>>,
}

impl<App: Application> Runtime<App> {
    /// Creates a new runtime with the given initialization flags.
    ///
    /// The flags reach [`Application::new`] when [`run`](Self::run) is
    /// awaited, not here: construction is inert, and any command `new`
    /// returns is dispatched at bootstrap (RFC 0011 INV-LC3).
    ///
    /// The delivery controls take their defaults; use
    /// [`with_config`](Self::with_config) to set them.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tears::prelude::*;
    /// # use ratatui::Frame;
    /// # struct MyApp;
    /// # enum Message {}
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = ();
    /// #     fn new((): ()) -> (Self, Command<Message>) { (MyApp, Command::none()) }
    /// #     fn update(&mut self, _msg: Message) -> Command<Message> { Command::none() }
    /// #     fn view(&self, _frame: &mut Frame<'_>) {}
    /// #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
    /// # }
    /// let runtime = Runtime::<MyApp>::new(());
    /// ```
    pub const fn new(flags: App::Flags) -> Self {
        Self {
            inner: ProgramRuntime::new(AppProgram::new(), flags),
        }
    }

    /// Creates a new runtime with explicit delivery controls.
    ///
    /// `config` is moved rather than copied, so a setter's return value
    /// cannot be discarded and the original silently reused (RFC 0007 §2.1).
    pub const fn with_config(flags: App::Flags, config: RuntimeConfig) -> Self {
        Self {
            inner: ProgramRuntime::with_config(AppProgram::new(), flags, config),
        }
    }

    /// Runs the application to termination, then settles.
    ///
    /// Returns `Ok(())` for a quit of either route and the backend's error
    /// for a render failure — the classification RFC 0011 INV-LC5 states,
    /// unchanged. The quiescent postcondition holds under both.
    ///
    /// # Errors
    ///
    /// Returns the backend's error when a render failed.
    pub async fn run<B: Backend>(self, terminal: &mut Terminal<B>) -> Result<(), B::Error> {
        self.inner.run(terminal).await.map(|Exit::Quit| ())
    }
}
