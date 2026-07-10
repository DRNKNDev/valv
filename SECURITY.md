# Security Policy

## Supported Versions

Only the latest published release of Valv (app, CLI, daemon, and backend) receives security fixes. Older releases are not patched; update to the latest release before reporting an issue that may already be fixed.

## Reporting A Vulnerability

**Do not open a public GitHub issue for a security vulnerability.**

Report it privately through GitHub's vulnerability reporting flow for this repository:

1. Go to the [Security tab](https://github.com/DRNKNDev/valv/security).
2. Click **Report a vulnerability**.
3. Fill in the advisory form.

This routes the report directly to maintainers and keeps it private until a fix is available.

### What To Include

To help triage the report quickly, include:

- Affected component (macOS app, CLI, daemon, or backend) and version.
- Platform and architecture.
- Steps to reproduce, including any required configuration.
- Expected behavior versus observed behavior.
- Impact you believe the vulnerability has (e.g. data exposure, privilege escalation, denial of service).
- Any logs or proof-of-concept, with secrets, tokens, and account identifiers redacted.

### What To Expect

We aim to acknowledge new reports promptly and will work with you to understand and confirm the issue. We do not commit to a fixed remediation deadline, since severity and complexity vary, but we will keep you updated as we investigate and prepare a fix.

### Coordinated Disclosure

Please give us a reasonable opportunity to investigate and release a fix before any public disclosure. We are happy to credit reporters in release notes or an advisory unless you prefer to remain anonymous.
