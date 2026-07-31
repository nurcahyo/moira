// The gate that makes a skipped database suite a RED run.
//
// Moira's own history is the argument: `MOIRA_TEST_DATABASE_URL` pointing at the
// wrong database silently skipped every database test and the run still reported
// success, which invalidated the gate it was supposed to be. So the console's
// database suite has exactly one way to be disabled, it is explicit, and taking
// it fails this file.
//
// Nothing here connects to anything. It only asserts that the run was not
// quietly downgraded.
import { describe, expect, test } from "bun:test";

import { databaseTestsSkipped, SKIP_ENV, testDatabaseUrl } from "../support/console-db";

describe("the database-backed suite actually ran", () => {
  test(`${SKIP_ENV} is not set`, () => {
    expect(
      databaseTestsSkipped(),
      `${SKIP_ENV} is set, so every database-backed test in this suite was skipped. ` +
        "Durability, migration and at-rest-leak coverage did not run. This failure is " +
        "deliberate: a suite that silently disables itself is worse than one that is red.",
    ).toBe(false);
  });

  test("the console's test database is not Moira's", () => {
    // One database, two independent migration ledgers, is the failure this
    // check exists to make unreachable. The console's schema is applied by
    // `console/db/migrate.ts`; Moira's by its own binary from the
    // repository-root `migrations/`.
    const consoleDatabase = new URL(testDatabaseUrl()).pathname;
    const moiraDatabase = new URL(
      process.env["MOIRA_TEST_DATABASE_URL"] ?? "postgres://x/moira",
    ).pathname;
    expect(
      consoleDatabase,
      "CONSOLE_TEST_DATABASE_URL points at Moira's database. They may share a server; " +
        "they may not share a database.",
    ).not.toBe(moiraDatabase);
  });
});
