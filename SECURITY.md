# Security policy

## Reporting a vulnerability

Please report security problems privately, not in a public issue: use **Security → Report a vulnerability** on the [repository page](https://github.com/paintballrefjosh/linecard/security/advisories/new). You'll get an answer within a few days. Please include the version, what you did, and what you saw.

## What is covered

Linecard, its sync protocol (`docs/sync-protocol.md`) and the relay (`docs/relay.md`). The sync design assumes the relay is untrusted: it should never see anything but encrypted data. A way around that is the kind of report most wanted.

## Supported versions

Only the latest release gets fixes. The app offers new versions from inside, and the relay image is published to `ghcr.io/paintballrefjosh/linecard-relay` with every release.

## How releases are protected

- Update files are signed with the project's key, and the app refuses ones that aren't.
- Relay images are signed with cosign (keyless); see `docs/relay.md` for the verification command.
- CI checks every change with clippy, tests on Windows, Linux and macOS, `cargo-deny` and `npm audit`, and CodeQL; Dependabot proposes dependency updates weekly.
