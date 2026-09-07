# Security Policy

AgentMux executes model-directed workflows around source code, local files, and
provider credentials. Security reports are welcome and should be handled
privately until a coordinated disclosure is ready.

## Supported versions

| Version | Supported |
| --- | --- |
| Current `main` branch / unreleased beta | Yes |
| Older snapshots and unmaintained forks | No |

This table will switch to explicit release series after the first public
release.

## Reporting a vulnerability

Use the repository's **Security → Report a vulnerability** action to open a
private GitHub Security Advisory. Do not file a public issue for exploitable
details. If private vulnerability reporting is unavailable, contact a
maintainer privately and disclose only enough information to establish a secure
reporting channel.

Include when possible:

- affected revision or release;
- impact and required attacker capabilities;
- minimal reproduction steps or a proof of concept;
- whether credentials, files, command execution, or provider data are exposed;
- suggested mitigation, if known;
- any disclosure deadline that already applies.

Never include real API keys, customer data, private repository contents, or
other people's personal data. Use synthetic values and redact diagnostic
bundles with `agentmux export` before attaching them.

## Response process

Maintainers aim to acknowledge a report within five business days. They will
validate the issue, agree on a communication channel, assess affected versions,
prepare tests and a fix, and coordinate release and disclosure with the
reporter. Complex or disputed reports may take longer; status updates should be
provided during investigation.

A public advisory should describe impact, affected versions, remediation, and
credit where requested. Exploit details may remain limited until users have had
a reasonable opportunity to update.

## Security boundaries

The following are security-sensitive and should be reported privately when a
boundary can be bypassed:

- workspace path and symlink confinement;
- patch digest and explicit approval enforcement;
- controlled command allowlists and execution grants;
- secret redaction or credential persistence;
- unauthorized delegation, depth, concurrency, or dependency bypass;
- provider request/response confusion that enables unintended tool execution;
- terminal escape injection;
- session isolation and recovery that misrepresents side effects.

Model hallucination, low-quality suggestions, expected provider billing, and
denied operations are generally not vulnerabilities unless they bypass an
enforced AgentMux boundary.

## Safe research

Test only systems and accounts you own or have permission to assess. Avoid
privacy violations, service disruption, destructive commands, persistence,
credential use, and access to unrelated data. Stop testing and report promptly
if sensitive data is encountered.
