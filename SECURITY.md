# Security Policy

## Supported Versions

Only the most recent 0.x release receives security fixes. Upgrade to the
latest release before reporting an issue against an older version.

## Reporting a Vulnerability

Report vulnerabilities privately, either via GitHub's private
vulnerability reporting (repository **Security** tab → *Report a
vulnerability*) or by email to the maintainer at
randymcmillan@protonmail.com.

Please include:

- the affected version and feature flags in use,
- a minimal reproduction,
- the observed and expected behavior.

Do not open a public issue for security reports.

## Security-Relevant Design Notes

See the crate-level documentation and README for the current security
posture: the registry cap (`unbounded` feature), the `explicit_name` /
`glob_support` name filters, `NameError` behavior, and the
`#![forbid(unsafe_code)]` guarantee.
