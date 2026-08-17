# Trademark policy

**The marks "Moira" and any associated logos are owned by PT Vayu Akasa Teknologi.**

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

The marks are held by **PT Vayu Akasa Teknologi**, an Indonesian limited liability company.
They are **not currently registered**. Common-law rights arise from use, and this policy is the
statement of that use. Registration may follow; this file will be updated if it does, and its
terms will not become more restrictive retroactively.

Three things a reader deserves to know rather than discover.

**"Moira" is a weak mark.** It is an ordinary word — the Greek Fates — and ordinary words are
harder to register and harder to enforce than invented ones. This policy is written to be useful
anyway, but nobody should mistake it for the protection an arbitrary or coined name would carry.

**Indonesia is first-to-file, and that changes the calculation.** Under Law No. 20 of 2016
Article 3, the party that registers a mark first is its lawful owner **even if someone else used
it first**. The common-law reasoning that makes "register later" safe in the United States does
not transfer here: in Indonesia, prior use is largely not a defence against a later registrant.

The widely-repeated advice to wait until after a funding round is sound in a first-to-use
jurisdiction and **wrong in this one**. Registration through DJKI costs roughly Rp500,000 to
Rp1,800,000 per class and takes about 10 to 15 months to process. Protection runs ten years and
renews. So the cost of registering early is small and known; the cost of waiting is that someone
else may hold the name by the time it is worth holding — and even a decision made today is not
protected for over a year.

**Writing this policy is not a substitute for registering.** The policy establishes what use
looks like and is worth having regardless. But in a first-to-file system it does not, by itself,
secure the mark.

**Contact:** open an issue at <https://github.com/nurcahyo/moira/issues>, or write to
PT Vayu Akasa Teknologi.

### One note on ownership, recorded because it is cheaper to fix now

The marks sit with PT Vayu Akasa Teknologi. The **copyright** in the code currently sits with the
individual author, and the Apache-2.0 grant in `LICENSE` is made on that basis.

That split is workable and common, but it is not automatic: a company does not acquire rights in
code simply by being the company. If PT Vayu Akasa Teknologi is to operate a commercial service
built on this software, it needs a licence or assignment from the copyright holder, recorded in
writing.

ClickHouse is the cautionary case. Its trademark was originally registered to Yandex and had to
be formally transferred to ClickHouse, Inc. years later, through recorded assignments. Nothing
went wrong — but the transfer was work that would have been trivial at the outset. Aligning the
holders now, while there is one author and no registrations, costs almost nothing; doing it after
contributors and registrations accumulate does not.
