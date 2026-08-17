# Trademark policy

**The Apache-2.0 licence covers the code. It does not cover the name.**

Section 6 of the licence is explicit about this: it *"does not grant permission to use the
trade names, trademarks, service marks, or product names of the Licensor."* This file states
what that means in practice, so that neither a contributor nor a downstream user has to guess.

## What you may do without asking

- **Run, modify, fork, and redistribute the software**, under the terms of `LICENSE`. That is
  the whole point of Apache-2.0 and nothing here restricts it.
- **Say truthfully that your product uses, is built on, is compatible with, or is derived from
  Moira.** Nominative use — naming the thing in order to refer to it — needs no permission.
- **Keep the name in an unmodified redistribution.**
- **Write about Moira**, review it, teach it, benchmark it, and criticise it.

## Running Moira as a service, including commercially

**This is permitted, and the licence says so.** Apache-2.0 grants it and nothing here takes it
back. What is reserved is the *name*, and the line follows the shape ClickHouse publishes for
the same situation — a shape that has been tested against a real competitor.

**Permitted, no permission needed.** Use these forms:

- *"XYZ is a fully managed cloud offering based on Moira software."*
- *"XYZ for Moira"* — for a compatible product, where the bundled binaries are this project's
  own and the APIs are current-compatible.
- *"Support for Moira software"* — **not** *"Moira Support"*, which reads as an offering of
  this project.
- A modified build may say *"This software is derived from the source code for Moira software"*
  — but must remove any logo.

Any of these should carry a plain statement that the provider is not this project and not
affiliated with it. That single sentence is what makes the rest work.

**Not permitted:**

- **"Managed Moira", "Hosted Moira", "MyMoira", "MoiraConnector"** — and **"Moira Cloud"**, which
  is reserved to this project. The test is whether the mark is the service's *identity* rather
  than a description of what it runs.
- Registering any **domain** containing the name or a variant of it.
- Using it as a **noun**, pluralising it (*"three Moiras"*), or hyphenating it
  (*"Moira-Gateway"*).
- Using a logo on a managed service, or on a modified build.
- Naming a **modified** version "Moira", or anything confusable with it — fork freely and give
  the fork its own name.

## Other things that need permission

- Using the name or any logo **as your own product identity**, whether or not a service is
  involved.
- Merchandise, or any use implying this project produced or endorsed the thing.

## Why this file exists, stated plainly

A permissive licence gives away every copyright-based control. **Trademark is the one control
that survives it**, and it is the mechanism by which "the official Moira" stays a meaningful
phrase.

This is not a restriction on use. Nothing here narrows the Apache-2.0 grant, and nothing here
prevents anyone from running Moira, hosting it, selling access to it, or competing with any
hosted offering. It restricts only **calling that thing Moira**.

If your intended use is not obviously covered above, ask rather than guess — the answer is
usually yes, and asking is cheaper for both sides than a dispute.

### The evidence this is shaped from

The structure above follows ClickHouse's trademark policy, which is the clearest working proof
that this approach holds. ClickHouse is Apache-2.0 and has never relicensed. Competitors —
including Yandex, who wrote it originally — openly sell managed ClickHouse. Asked directly in
2022 whether the company would relicense to shut them out, its co-founder answered *"I don't see
it as a reasonable move."* Four years later it still hasn't, and the resellers comply with the
naming rules voluntarily rather than through litigation.

Elastic's case is the counterweight and points the same way: changing the licence did not stop
AWS, which simply forked OpenSearch. **The trademark suit did.** After settlement, Amazon renamed
its service, and Elastic Cloud became the only "Elasticsearch" service on AWS Marketplace.

The lesson both point at is the same: **the licence governs the code, and the name is what
governs the market.** Apache-2.0 §6 leaves that lever untouched on purpose.

## Status, stated honestly

The name is **not currently a registered trademark**. Common-law rights arise from use, and this
policy is the statement of that use. Registration may follow; this file will be updated if it
does, and its terms will not become more restrictive retroactively.

Two things a reader deserves to know rather than discover:

- **"Moira" is a weak mark.** It is an ordinary word — the Greek Fates — and ordinary words are
  harder to register and harder to enforce than invented ones. This policy is written to be
  useful anyway, but nobody should mistake it for the protection an arbitrary or coined name
  would carry.
- **Registration is cheap and worth doing early.** PostHog reports securing their mark across
  four territories for roughly USD 3,000 in total, and advises founders to do it after a seed
  round rather than before. That is the order of magnitude, not a quote.

The reason to write this file before registering anything: the policy is the evidence of use,
and use is what common-law rights are built from. Waiting for registration to say what the rules
are means having no rules during the period when the name is easiest to take.

**Contact:** open an issue at <https://github.com/nurcahyo/moira/issues>.
