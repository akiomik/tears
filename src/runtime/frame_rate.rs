//! Frame rate value object for runtime scheduling.

use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::num::NonZeroU32;
use std::result::Result as StdResult;
use std::time::Duration;

/// Target frames per second for [`Runtime`](crate::Runtime).
///
/// The value must be non-zero and small enough to produce a non-zero frame
/// duration. This prevents divide-by-zero panics and invalid zero-duration
/// Tokio intervals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameRate {
    frames_per_second: NonZeroU32,
}

impl FrameRate {
    /// Highest supported frame rate.
    ///
    /// `Duration::from_secs(1) / MAX` is exactly one nanosecond. Larger values
    /// round down to a zero duration and cannot be used with Tokio intervals.
    pub const MAX: u32 = 1_000_000_000;

    /// Creates a frame rate from a non-zero FPS value.
    ///
    /// # Errors
    ///
    /// Returns [`FrameRateError::TooHigh`] when the value is greater than
    /// [`FrameRate::MAX`].
    pub const fn new(frames_per_second: NonZeroU32) -> StdResult<Self, FrameRateError> {
        let value = frames_per_second.get();
        if value <= Self::MAX {
            Ok(Self { frames_per_second })
        } else {
            Err(FrameRateError::TooHigh {
                value,
                max: Self::MAX,
            })
        }
    }

    /// Returns the frame rate in frames per second.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.frames_per_second.get()
    }

    pub(crate) fn frame_duration(self) -> Duration {
        Duration::from_secs(1) / self.get()
    }
}

/// Error returned when constructing an invalid [`FrameRate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameRateError {
    /// Frame rate is too high to produce a non-zero frame duration.
    TooHigh {
        /// Provided frame rate.
        value: u32,
        /// Maximum supported frame rate.
        max: u32,
    },
}

impl Display for FrameRateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::TooHigh { value, max } => {
                write!(f, "frame rate must be at most {max}, got {value}")
            }
        }
    }
}

impl Error for FrameRateError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_rate(value: u32) -> FrameRate {
        FrameRate::new(NonZeroU32::new(value).expect("frame rate must be non-zero"))
            .expect("frame rate must be valid")
    }

    #[test]
    fn test_frame_rate_new() {
        assert_eq!(frame_rate(60).get(), 60);
        assert_eq!(
            FrameRate::new(NonZeroU32::new(FrameRate::MAX + 1).expect("non-zero")),
            Err(FrameRateError::TooHigh {
                value: FrameRate::MAX + 1,
                max: FrameRate::MAX,
            }),
        );
    }

    #[test]
    fn test_frame_rate_frame_duration_bounds() {
        assert_eq!(
            frame_rate(FrameRate::MAX).frame_duration(),
            Duration::from_nanos(1)
        );
    }
}
