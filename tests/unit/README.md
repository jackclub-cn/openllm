# Unit Tests

Files in this directory are included by their owning source modules with
`#[cfg(test)]` and `#[path]`. This keeps unit tests together while preserving
access to private implementation details.

Integration tests that only exercise the public API should live directly in the
top-level `tests/` directory so Cargo builds them as separate crates.
