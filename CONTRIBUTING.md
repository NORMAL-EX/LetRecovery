# Contributing

LetRecovery is a high-privilege Windows disk tool. Compatibility, recovery,
and prevention of wrong-disk writes take priority over refactoring speed.

## Development environment

- Rust 1.88 or newer; CI uses Rust 1.88.0.
- Visual Studio Build Tools 2022 with Desktop development with C++ and a
  Windows 10/11 SDK.
- Node.js 22 and npm for changes under `官网/`.
- Build from the committed `Cargo.lock`; do not regenerate it without a real
  dependency change.
- CI sets `SOURCE_DATE_EPOCH` to the checked-out commit timestamp. Set the same
  value locally when reproducing a release binary; without it, local builds
  retain the historical behavior of using the current UTC date as file version.

## Required checks

Run from the repository root:

```text
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked
cargo test --workspace --no-run --locked --features "LetRecovery/non-elevated-tests,letrecovery-pe/non-elevated-tests"
cargo test -p lr-core --locked
cargo test -p letrecovery-pe --locked --features non-elevated-tests
cargo test -p LetRecovery --locked --features non-elevated-tests
```

The endpoint feature `non-elevated-tests` changes only non-release test/debug
manifests from `requireAdministrator` to `asInvoker`, so CI can launch the test
harness without a UAC prompt. Both endpoint build scripts reject this feature
in release builds. Tests that inspect a real Windows installation or host disk
state must be marked `#[ignore]` with a reason and run manually only in a
disposable VM. CI must not use `--ignored`.

For website changes, also run from `官网/`:

```text
npm ci
npm run lint
npm run type-check
npm run build
```

Record environment-specific blockers accurately. A command that could not run
is not a passing test.

## Destructive-operation rule

Automated tests and CI must never execute real `format`, DiskPart, DISM writes,
BCD changes, partition changes, registry injection, reboot, or disk-image
restore operations. Test command construction and policy with pure functions,
mocks, temporary regular files, or dry-run adapters. Hardware and VM validation
must use disposable images and dedicated test disks outside the automated test
suite.

## Change discipline

- Keep behavior-compatible changes small and reviewable.
- Validate drive letters, filesystem names, labels, URLs, and server-provided
  paths before they reach operating-system commands.
- Invoke programs directly with separate arguments whenever possible. A shell
  wrapper requires an explicit compatibility reason and stricter validation.
- Route new process execution through `lr_core::command::CommandRequest` and a
  `CommandExecutor`. Exercise destructive command paths with
  `DryRunCommandExecutor`; never spawn the real tool from a unit test.
- Preserve both normal-system and WinPE behavior unless the change explicitly
  targets one environment.
- Add English translations in `assets/release/lang/en-US.json` for new visible
  Chinese strings.
- Update `docs/THIRD_PARTY_BINARIES.md` and the retained upstream license files
  whenever a bundled binary changes.

Contributions are accepted under the repository's
[PolyForm Noncommercial License 1.0.0](LICENSE).
