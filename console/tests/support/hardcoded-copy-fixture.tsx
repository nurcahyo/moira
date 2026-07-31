// POSITIVE CONTROL for `tests/unit/lib/no-hardcoded-copy.test.tsx`.
//
// This file is DELIBERATELY WRONG. Every violation the scanner is supposed to
// find is present here exactly once, and the test asserts that all four are
// reported. Without it the suite proves only that the scanner ran to completion
// over a clean tree — which is the same thing a regex that stopped matching
// would prove, and is precisely the shape of the earlier "metrics assertion
// matching nothing because of a global label" finding in this project.
//
// It lives under `tests/support/` so the real scan set (`components/**`,
// `modules/**`) never sees it; the test points the scanner at this one file
// explicitly. DO NOT "fix" the strings below.

/** Violation 1 + 2: a hardcoded default parameter value, and JSX text. */
export function FixtureBanner({ heading = "Something went wrong" }: { heading?: string }) {
  return (
    <section>
      <h2>{heading}</h2>
      {/* Violation 2: literal text in JSX text position. */}
      <p>Contact your administrator before retrying this operation.</p>
      {/* Violation 3: a literal in an accessibility-bearing prop. */}
      <button type="button" aria-label="Dismiss this banner">
        <span aria-hidden="true">×</span>
      </button>
      {/* Violation 4: a literal `title` prop, and a literal `alt`. */}
      {/* eslint-disable-next-line @next/next/no-img-element -- a fixture, never rendered */}
      <img src="/fixture.png" alt="A diagram of the failure" title="Failure diagram" />
    </section>
  );
}
