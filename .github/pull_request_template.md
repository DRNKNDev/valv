<!--
Thanks for contributing to Valv. See CONTRIBUTING.md for setup, verification
commands, and how public pull requests are integrated through the private
mirror workflow.
-->

## Summary

<!-- What does this change do, and why? -->

## Scope

- [ ] I read [`CONTRIBUTING.md`](../CONTRIBUTING.md).
- [ ] This PR touches (check all that apply): TypeScript contracts / core / e2e / Rust crates / macOS app / File Provider / DaemonKit / docs / CI

## Verification

<!-- List the commands you ran and their outcome. Only include the ones relevant to your change. -->

- [ ] `pnpm typecheck`
- [ ] `pnpm test:core`
- [ ] `pnpm test:e2e`
- [ ] `cargo check --workspace` / `cargo test --workspace` (from `crates/`)
- [ ] `./e2e/smoke/run-all.sh`
- [ ] `swift test --package-path macos/DaemonKit`
- [ ] macOS app/extension build in Xcode

## Documentation And Spec Impact

- [ ] This change updates relevant README(s)/docs.
- [ ] This change affects a documented OpenSpec requirement/capability (note which one).
- [ ] No documentation or spec impact.

## Security

- [ ] This PR does not introduce hardcoded secrets, tokens, or private account data.
- [ ] Any attached logs/screenshots have secrets and identifying data redacted.
- [ ] This PR is not a vulnerability report (vulnerabilities go through [private reporting](../SECURITY.md), not a pull request).
