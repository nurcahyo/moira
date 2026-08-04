// The one definition of what "an accessibility violation that fails the gate"
// means, shared by every spec that runs axe.
//
// Extracted when the setup wizard got its own audited surfaces
// (`setup-wizard.e2e.ts`): two files each carrying their own tag list and their
// own impact threshold is two gates that can silently disagree about what they
// enforce — and the weaker one is the one nobody notices.

import AxeBuilder from "@axe-core/playwright";
import type { Page, TestInfo } from "@playwright/test";
import type { Result } from "axe-core";

/** CONVENTIONS.md §3: WCAG 2.0/2.1 level A and AA. */
export const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

/** Plan 08: "zero critical/serious violations gates CI". */
export const BLOCKING_IMPACTS = new Set(["critical", "serious"]);

export interface AxeAudit {
  /** Only the violations whose impact blocks the gate. */
  readonly blocking: readonly Result[];
  /** The blocking ids, for the assertion itself. */
  readonly ids: readonly string[];
  /** A human-readable report of `blocking`, for the failure message. */
  readonly report: string;
}

/**
 * Run axe over the page as it currently stands, and attach the full violation
 * list to the test result.
 *
 * `label` names the SURFACE, not the route: a wizard step is a distinct thing to
 * audit even though every step shares one URL, and an attachment named after the
 * URL alone would overwrite the previous step's evidence.
 */
export async function auditPage(page: Page, testInfo: TestInfo, label: string): Promise<AxeAudit> {
  const results = await new AxeBuilder({ page }).withTags(AXE_TAGS).analyze();

  await testInfo.attach(`axe-${label.replace(/\W+/g, "_")}.json`, {
    body: JSON.stringify(results.violations, null, 2),
    contentType: "application/json",
  });

  const blocking = results.violations.filter((violation) =>
    BLOCKING_IMPACTS.has(violation.impact ?? ""),
  );

  const report = blocking
    .map(
      (violation) =>
        `  [${violation.impact}] ${violation.id}: ${violation.help}\n` +
        violation.nodes.map((node) => `      ${node.target.join(" ")}`).join("\n") +
        `\n      ${violation.helpUrl}`,
    )
    .join("\n");

  return { blocking, ids: blocking.map((violation) => violation.id), report };
}
