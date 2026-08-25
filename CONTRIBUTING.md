# Contributing

## The bar

Every change is proof-backed:

- A claim gets a test that would fail if the claim were false. Doc comments that say "never" or
  "always" are claims. So are design decisions and assumptions about performance.
- After a test passes, break the code and confirm the test notices. A test that stays green when
  the behaviour it guards is removed is not a test.
- Anything with a face is checked in the shipped binary, not only in a library test. Drive the
  window and the CLI for real.
- If a property cannot be tested in this suite, say so at the site, so nobody reads the tests as
  covering it.

## Before you push

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must be clean. On Windows, stop any running `botroster.exe` or `botroster-app.exe` first;
a held binary makes `cargo build` fail with "Access is denied", and the tests then run against a
stale one.

## Commit messages

Long is fine. Record the reasoning, the alternatives you rejected, and what you measured.

## What is not merged

- Code from elsewhere without a row in [`PROVENANCE.md`](PROVENANCE.md).
- A test that cannot fail.
- A change that weakens either structural invariant: `botroster-guest` must never be able to reach
  `botrosterd` (`crates/botroster-guest/tests/isolation.rs`), and the policy gate stays in the hub.

## Licence

Contributions are licensed under the [Apache-2.0](LICENSE) licence that covers the project.
