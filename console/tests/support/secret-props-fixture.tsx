// POSITIVE CONTROLS for `tests/unit/architecture/no-secret-props.test.ts`.
//
// This file is DELIBERATELY WRONG, and it is the only reason that guard is worth
// anything on the day it lands: the real tree has seven `*Props` interfaces and
// zero secret-shaped members, so "violations === []" is true whether the scanner
// works or not.
//
// It lives under `tests/support/` so the real scan roots (`components/atoms/**`,
// `components/molecules/**`, `modules/**`) never see it. DO NOT "fix" it.

import type { ReactNode } from "react";

/** Positive control for rule (a): a secret-shaped prop name. */
export interface FixtureLeakyProps {
  label: string;
  /** The violation. */
  clientSecret: string;
  children?: ReactNode;
}

/** Positive control for rule (a), second spelling. */
export interface FixtureTokenProps {
  readonly inviteToken: string;
}

/** A clean interface, so the scanner is shown not to flag everything. */
export interface FixtureCleanProps {
  label: string;
  expiresAt: string;
  onDismiss: () => void;
}

/** Positive control for rule (b): the secret handed to a child as a prop. */
export function FixtureFlowByName({ secret }: { secret: string }) {
  return <FixtureChild secret={secret} />;
}

/** Positive control for rule (b): the secret under a different prop name. */
export function FixtureFlowByValue({ secret }: { secret: string }) {
  return <FixtureChild value={secret} />;
}

/** Positive control for rule (b): the secret through a spread. */
export function FixtureFlowBySpread({ secret }: { secret: string }) {
  return <FixtureChild {...{ secret }} />;
}

/** Negative control for rule (b): the secret rendered, never forwarded. */
export function FixtureFlowClean({ secret }: { secret: string }) {
  return (
    <FixtureChild>
      <code>{secret}</code>
    </FixtureChild>
  );
}

function FixtureChild(props: { secret?: string; value?: string; children?: ReactNode }): ReactNode {
  return props.children ?? null;
}
