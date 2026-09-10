<!-- What changes, and why. -->

- [ ] `CHANGELOG.md` has an entry under `## [Unreleased]`, or this change needs
      none — CONTRIBUTING.md says which changes do
- [ ] This breaks nothing a dependant relies on today, or — where it does,
      whether by a compile error, by changed behaviour, or by raising
      `rust-version` — the commit subject carries `!` before the `:` and the
      changelog entry opens `**Breaking:**`

Pre-1.0, a break here makes the next release a minor.

See [CONTRIBUTING.md](https://github.com/akiomik/tears/blob/main/CONTRIBUTING.md).
