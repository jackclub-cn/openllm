# Project Working Rules

Read [docs/CODE_STANDARDS.md](docs/CODE_STANDARDS.md) before changing code.
These rules apply to the whole repository.

- Keep each module focused on one responsibility. Extend the owning domain
  module instead of appending unrelated code to an entry point or a helper file.
- Check file size before and after editing. Split by responsibility during the
  same task when a file would exceed the limits below; do not wait for a separate
  cleanup request.
- Target 500 lines for Rust and Rust tests, and 300 lines for TypeScript, TSX,
  JavaScript and CSS. Hard limits are 1000 and 600 lines respectively.
- Existing files above a hard limit are listed in `scripts/code-size-baseline.json`.
  They must not grow. Reduce their baseline when shrinking them and remove their
  entry once compliant. Do not raise limits or add exceptions to bypass a failure.
- Keep module facades small. Rust child modules may use the existing `use super::*`
  pattern inside one domain; do not widen visibility outside that domain.
  Keep the frontend `api.ts` facade stable and place wire types in `api/`.
- Put tests next to their owning domain under `tests/unit/<domain>/`. Keep shared
  fixtures in the suite entry point or a named support module. Do not concatenate
  tests with `include!` or create numbered chunks just to meet a line limit.
- Preserve user changes, existing fixes, API contracts, routing defaults and
  migration order during structural work. Run the complete Rust suite after
  moving shared modules or tests and confirm the test count did not fall.
- Run `node scripts/check-code-structure.mjs` and `cargo fmt --check` before
  finishing. For Rust changes also run `cargo test --locked` and
  `cargo clippy --locked --all-targets -- -D warnings`. Run `npm run build` in
  `web/` for frontend changes and the Node test suite when editing structure
  checks. Report checks that could not pass.
- Update architecture documentation when module ownership changes. Avoid new
  dependencies for code organization or checks when the existing toolchain works.
- Commit, tag and publish only when requested. Do not introduce `auto/*` routing
  or enable latency penalties on unrelated routing strategies.
- When a release is requested, follow [docs/RELEASING.md](docs/RELEASING.md)
  through version updates, validation, tag CI and release artifact verification.
