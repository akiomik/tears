# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- WebSocket subscription support for real-time bi-directional communication
  - New `subscription::websocket::WebSocket` subscription source (requires `ws` feature)
  - Supports both secure (wss://) and insecure (ws://) connections
  - TLS backend options:
    - `native-tls` - Platform's native TLS implementation
    - `rustls` - Pure Rust TLS with ring crypto provider and native root certificates
    - `rustls-tls-webpki-roots` - Pure Rust TLS with ring crypto provider and webpki root certificates
  - Automatic connection management and reconnection handling
  - Streams all WebSocket message types (Text, Binary, Ping, Pong, Close)
  - Example: `examples/websocket.rs` demonstrating WebSocket echo chat
  - Comprehensive documentation with usage examples and TLS configuration guide

## [0.4.0] - 2026-01-03

### Changed

- **BREAKING**: Improved `Timer` subscription performance and accuracy
  - Migrated from `tokio::time::sleep` to `tokio::time::interval` for better timing accuracy
  - Uses `MissedTickBehavior::Skip` to maintain consistent tick rate (drops missed ticks instead of catching up)
  - Provides drift correction for high frame rates (60+ FPS)
  - Added `tokio-stream` dependency for interval stream support
- Improved `Runtime` frame timing accuracy
  - Migrated from `tokio::time::sleep` to `tokio::time::interval` for consistent frame rate
  - Uses `MissedTickBehavior::Skip` to skip missed frames when rendering takes longer than frame duration
  - Provides more accurate and stable FPS delivery

## [0.3.0] - 2026-01-02

### Fixed

- Fixed signal subscriptions emitting spurious events on initialization

### Removed

- **BREAKING**: Removed `ignore_initial()` method from signal subscriptions (`Signal`, `CtrlC`, `CtrlBreak`) as it is no longer needed after fixing the spurious event bug

## [0.2.0] - 2026-01-02

### Added

- Signal subscription helpers
  - `ignore_initial()` method to filter spurious signals during TUI initialization

### Changed

- **BREAKING**: Signal subscriptions refactored for simplicity
  - `Signal`, `CtrlC`, and `CtrlBreak` include grace period configuration
  - Removed `Copy` implementation from signal types (not needed in practice)
- Initialized `color_eyre` in example applications for improved error reporting and debugging experience

## [0.1.1] - 2026-01-02

### Added

- Signal subscription support for handling OS signals
  - Unix: `signal::Signal` for SIGINT, SIGTERM, SIGHUP, SIGQUIT, and 20+ other signals
  - Windows: `signal::CtrlC` and `signal::CtrlBreak` for Ctrl+C and Ctrl+Break events
  - Uses `tokio::signal` types directly (no unnecessary wrappers)
  - Returns `Result<(), io::Error>` for proper error handling
  - Example: `examples/signals.rs` demonstrating signal handling with graceful shutdown

### Fixed

- Downgraded `crossterm` from 0.29 to 0.28 to match `ratatui`'s dependency requirements

## [0.1.0] - 2025-12-30

### Added

- Initial release of tears framework
- Core Elm Architecture implementation with `Application` trait
- Asynchronous command system with `Command` type
  - `Command::none()` - No-op command (const fn)
  - `Command::single(msg)` - Send a single message immediately
  - `Command::perform()` - Execute async operation with result transformation
  - `Command::future()` - Execute async operation that produces a message
  - `Command::effect()` - Execute an action immediately
  - `Command::batch()` - Execute multiple commands concurrently
  - `Command::stream()` - Create command from message stream
  - `Command::run()` - Transform and consume a stream
- Subscription system for event sources
  - Dynamic subscriptions (can change based on application state)
  - Built-in `TerminalEvents` subscription for keyboard/mouse/resize events
  - Built-in `Timer` subscription for periodic ticks
  - Support for custom subscriptions via `SubscriptionSource` trait
- `Runtime` for managing application lifecycle
  - Event loop with configurable frame rate
  - Automatic subscription management
  - Command execution and message dispatching
- Error handling
  - Subscriptions return `Result<T, E>` allowing user-controlled error handling
  - Terminal event errors are propagated to application
- Full async/await support with tokio
- Comprehensive API documentation with examples
- Counter example demonstrating timer and keyboard input
