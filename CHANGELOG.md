# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## v0.1.0

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
