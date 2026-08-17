# Security policy

Moira sits on the credential path. It resolves provider credentials, holds identity claims, and
routes tenant traffic — so a defect here can expose material belonging to someone who never chose
to trust us directly. Reports are taken seriously and answered.

## Reporting a vulnerability

**Do not open a public issue.**

Use GitHub's private vulnerability reporting:
[**Report a vulnerability**](https://github.com/nurcahyo/moira/security/advisories/new).

The report is visible only to the maintainer until an advisory is published. If that form is
unavailable to you, open a public issue containing **no technical detail** — just a request for a
private channel — and you will be given one.

### What helps

- The affected version or commit, and the deployment shape (self-hosted, container, Helm).
- Which surface is reachable: the public execution API, the admin API, the console, or a worker.
- Whether tenant isolation is crossed, and if so in which direction.
- A reproduction, or the reasoning if you have not run it. **Say which one it is** — a source-review
  finding is welcome and is not diminished by being unexecuted, but it must not be presented as
  reproduced.

### What to expect

| | |
|---|---|
| First response | within 5 working days |
| Assessment and severity | within 10 working days |
| Fix, or a dated plan | within 30 days for high and critical |
| Credit | offered by default; say if you would rather stay anonymous |

Moira is maintained by one person. If a deadline above slips you will be told it slipped and why,
rather than left waiting.

## Scope

**In scope** — anything in this repository: the Rust service, the Next.js console, the migrations,
the Helm chart, the container images, and the CI workflows.

Findings that are always in scope, because they are the properties this project exists to hold:

- **Cross-tenant access** of any kind — reading, writing, enumerating, or inferring.
- **Credential exposure** — a provider secret reaching a log, a response body, an error message, a
  metric label, or a stack trace.
- **Fail-open behaviour** where the code claims to fail closed. A tenant request silently answered
  by a platform-owned credential is a vulnerability, not a convenience.
- **A control that reports healthy while enforcing nothing.** These are worse than an absent
  control, because they suppress the alarm that would otherwise be raised.
- **Stored content escaping its persistence policy**, including through derived artefacts such as
  embeddings, digests, or cache side effects.

**Out of scope**

- Vulnerabilities in an upstream provider's API. Report those to the provider.
- Findings that require an already-compromised host or database.
- Missing hardening headers on a page that serves no authenticated content, absent a demonstrated
  impact.
- Automated scanner output submitted without a reachable path through this codebase.

## Supported versions

Moira has not cut a stable release. Until it does, only the `develop` branch is supported, and
fixes land there.

## Disclosure

Advisories are published once a fix is available, or once a workaround is documented and the
window agreed with the reporter has passed. An advisory states plainly whether the finding was
reproduced or derived from source review, because the two carry different weight and conflating
them misleads operators deciding how urgently to patch.

Some advisories are drafted before a fix exists, so that the finding is recorded and tracked rather
than held in one person's memory. A draft advisory is not evidence of an exploited system.
