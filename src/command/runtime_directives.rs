#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RedrawPolicy {
    Redraw,
    Skip,
}

impl RedrawPolicy {
    const fn requests_redraw(self) -> bool {
        matches!(self, Self::Redraw)
    }

    const fn combine(self, other: Self) -> Self {
        if self.requests_redraw() || other.requests_redraw() {
            Self::Redraw
        } else {
            Self::Skip
        }
    }
}

// Runtime directives are owned and composed by `Command`; the runtime only
// reads the folded result after update processing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RuntimeDirectives {
    redraw: RedrawPolicy,
}

impl RuntimeDirectives {
    pub(super) const DEFAULT: Self = Self {
        redraw: RedrawPolicy::Redraw,
    };

    pub(super) const fn requests_redraw(self) -> bool {
        self.redraw.requests_redraw()
    }

    pub(super) const fn without_redraw(mut self) -> Self {
        self.redraw = RedrawPolicy::Skip;
        self
    }

    pub(super) const fn combine(self, other: Self) -> Self {
        Self {
            redraw: self.redraw.combine(other.redraw),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redraw_policy_combine_redraw_wins() {
        assert_eq!(
            RedrawPolicy::Skip.combine(RedrawPolicy::Skip),
            RedrawPolicy::Skip
        );
        assert_eq!(
            RedrawPolicy::Redraw.combine(RedrawPolicy::Skip),
            RedrawPolicy::Redraw
        );
        assert_eq!(
            RedrawPolicy::Skip.combine(RedrawPolicy::Redraw),
            RedrawPolicy::Redraw
        );
        assert_eq!(
            RedrawPolicy::Redraw.combine(RedrawPolicy::Redraw),
            RedrawPolicy::Redraw
        );
    }

    #[test]
    fn test_runtime_directives_combine_folds_redraw_policy() {
        let redraw = RuntimeDirectives::DEFAULT;
        let skip = RuntimeDirectives::DEFAULT.without_redraw();

        assert!(redraw.requests_redraw());
        assert!(!skip.requests_redraw());
        assert!(redraw.combine(skip).requests_redraw());
        assert!(skip.combine(redraw).requests_redraw());
        assert!(!skip.combine(skip).requests_redraw());
    }
}
