# Security Policy

LetRecovery runs with elevated privileges and can modify partitions, boot data,
offline registries, and operating-system images. Treat unexpected behavior in
these areas as potentially security-relevant.

## Supported versions

Security fixes target the latest release and the current `main` branch. Older
release packages may not receive backports.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting entry under the repository's
**Security** tab when it is available. Include:

- the affected version or commit;
- the normal-system or WinPE environment;
- reproducible steps that avoid unnecessary destructive operations;
- relevant logs with passwords, tokens, BitLocker recovery keys, personal
  paths, and machine identifiers removed;
- the expected impact and any known workaround.

Do not publish exploit details, credentials, recovery keys, or destructive
proof-of-concept steps in a public issue. If private reporting is unavailable,
open a minimal issue asking the maintainer to establish a private contact
channel, without including sensitive details.

## Scope

Examples include command injection, unsafe path handling, download integrity
bypass, incorrect target-disk selection, privilege-boundary mistakes, and
failures that can silently write to the wrong disk. General support requests
and ordinary reproducible bugs can use GitHub Issues.
