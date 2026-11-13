# Security policy

kumoba runs untrusted code, so isolation bugs matter more here than in most projects. Thank you for reporting them privately.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting: open the repository's **Security** tab and choose **Report a vulnerability**. Please do not open a public issue or pull request for a security problem.

Include what an attacker controls (guest code, an HTTP client of `kumobad`, a network peer), what they gain, and the smallest reproduction you have. You will get an acknowledgement within a week, and a fix or a plan once the report is confirmed. Credit goes in the release notes unless you prefer otherwise.

## Scope

In scope: escaping a guest to the host, reaching another sandbox, bypassing the egress allowlist or the private-address blocks, reading a secret's value from inside a guest, forging or reusing preview links, and authentication bypass on the `kumobad` API.

Known limitation: browser previews from all sandboxes share one origin (`127.0.0.1:7789` by default), so web storage written by one sandbox's page is readable by another's. Per-sandbox origins are planned.

Out of scope: denial of service by a client that holds the bearer token, and attacks that need the host's root account. libkrun and libkrunfw issues belong upstream at [containers/libkrun](https://github.com/containers/libkrun), though a heads-up here is welcome.

## Supported versions

Before 1.0 only the latest release gets security fixes.
