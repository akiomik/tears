# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `Command::message()` - Send a message to the application immediately
  - This is a tears-specific feature for immediate message dispatch
  - Replaces `Command::single()` with a clearer name that better represents the operation
  - More explicit than `single` and reserves `send`/`dispatch`/`emit` for future extensions

### Changed

- **BREAKING**: `Command::single()` has been removed
  - Use `Command::message()` instead for sending messages immediately
  - This change aligns with iced v0.14.0 design principles while maintaining tears' self-messaging feature
- Simplified `Runtime` internals by removing `Instance` wrapper
  - `Runtime` now directly holds the application instead of wrapping it in `Instance<App>`
  - Eliminates unnecessary indirection (`.inner`) throughout the codebase
  - Improves code readability with no functional changes
- Improved error handling in `examples/counter.rs`
  - `Message::TerminalError` now holds `io::Error` instead of `String`
  - Preserves full error information instead of converting to string
  - Removed unnecessary `Clone` derives from `Message` and `Counter`

### Migration Guide (v0.5.0 → v0.6.0)

#### Command API Changes

Replace all uses of `Command::single()` with `Command::message()`:

```rust
// Before (v0.5.0)
Command::single(Message::Refresh)

// After (v0.6.0)
Command::message(Message::Refresh)
```

This is a mechanical replacement with identical functionality.
The new name better clarifies the intent and reserves more generic verbs (`send`, `dispatch`, `emit`) for potential future features.

## [0.5.0] - 2026-01-04

### Changed

- Upgraded `ratatui` from 0.29 to 0.30
- Upgraded `crossterm` from 0.28 to 0.29
- Updated MSRV (Minimum Supported Rust Version) from 1.85.0 to 1.86.0
- Updated `Runtime::render` and `Runtime::run` return types to use generic backend error types
- **BREAKING**: WebSocket subscription now supports bidirectional communication
  - Added `WebSocketCommand` enum for sending messages (`SendText`, `SendBinary`, `Close`)
  - Added `WebSocketMessage` enum for subscription output (`Connected`, `Disconnected`, `Received`, `Error`)
  - `WebSocket` subscription now emits `WebSocketMessage` instead of raw `Message`
  - `WebSocketMessage::Connected` provides command sender when successfully connected
  - `WebSocketMessage::Disconnected` is emitted on normal connection closure
  - `Message::Close` frames are handled internally and result in `Disconnected` event
  - Single WebSocket connection handles both receiving and sending
  - Updated `examples/websocket.rs` to demonstrate bidirectional communication

## [0.4.1] - 2026-01-04

### Added

- Mock subscription source for deterministic testing
  - New `subscription::mock::MockSource` for controllable event emission in tests
  - Enables testing without real I/O or time dependencies
  - Shared (cloneable) design allows use in both application code and test code
  - Based on `tokio::sync::broadcast` for efficient multi-receiver support
  - Comprehensive documentation with testing examples in README.md
  - Added `sync` feature to `tokio-stream` dependency for broadcast stream support
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
