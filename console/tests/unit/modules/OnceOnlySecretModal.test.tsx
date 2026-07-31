// The once-only secret modal.
//
// ============================================================================
// THE ASSERTION THAT MATTERS IS THE ONE NAME-SCANNING CANNOT MAKE
// ============================================================================
//
// A test that renders the modal and asserts the token is on screen passes
// whether or not the same string ALSO went into a sibling component's props.
// `tests/unit/architecture/no-secret-props.test.ts` rule (b) scans this file's
// subject for three spellings of that mistake, and its own header admits it is a
// heuristic that a rename through an intermediate defeats.
//
// So the property is expressed at RUNTIME here: every imported component is
// wrapped in a recorder that captures the props object it was called with, and
// the test asserts that no recorded prop value anywhere equals the secret
// string. That covers any spelling, including ones nobody thought of, because it
// observes the values rather than the source text.
//
// The wrappers are TRANSPARENT — they record and then render the real component
// — so every other assertion in this file exercises the shipped behaviour.

import { afterEach, describe, expect, mock, test } from "bun:test";
import { createElement } from "react";
import { cleanup, render, screen } from "@testing-library/react";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { AdminInviteRecord, ResponseText } from "@/lib/types";

/* -------------------------------------------------------------------------- */
/* The recorder                                                               */
/* -------------------------------------------------------------------------- */

interface RecordedRender {
  readonly component: string;
  readonly props: Record<string, unknown>;
}

const recorded: RecordedRender[] = [];

const realCopyButton = await import("@/components/atoms/CopyButton");
const realDialog = await import("@/components/atoms/Dialog");
const realButton = await import("@/components/atoms/Button");

function recordingWrapper<P extends Record<string, unknown>>(
  name: string,
  real: (props: P) => unknown,
) {
  const Recording = (props: P) => {
    recorded.push({ component: name, props });
    return createElement(real as never, props as never);
  };
  // Named, so React DevTools and any error boundary report the real component
  // rather than "Anonymous" — and so `react/display-name` is satisfied without
  // disabling it.
  Recording.displayName = name;
  return Recording;
}

mock.module("@/components/atoms/CopyButton", () => ({
  ...realCopyButton,
  CopyButton: recordingWrapper("CopyButton", realCopyButton.CopyButton as never),
}));
mock.module("@/components/atoms/Dialog", () => ({
  ...realDialog,
  Dialog: recordingWrapper("Dialog", realDialog.Dialog as never),
}));
mock.module("@/components/atoms/Button", () => ({
  ...realButton,
  Button: recordingWrapper("Button", realButton.Button as never),
}));

// Imported AFTER the mocks are installed, or the modal would have captured the
// real bindings and the recorder would observe nothing — which would make every
// assertion below vacuous in the most flattering possible way.
const { OnceOnlySecretModal } = await import("@/modules/secrets/OnceOnlySecretModal");

/* -------------------------------------------------------------------------- */

/** The catalog's English. Never a literal in this file — see SignInPanel.test. */
const copy = (key: ConsoleMessageKey): string => CONSOLE_CATALOG[key].message;

/** High-entropy, and unmistakable if it turns up somewhere it should not. */
const TOKEN = "moira-invite-token-fixture-8c41ab07f2de9536";

const RECORD: AdminInviteRecord = {
  id: "11111111-2222-3333-4444-555555555555",
  constraint: "email",
  value: "operator@example.com",
  status: "pending",
  expired: false,
  expires_at: "2026-08-01T00:00:00Z",
  created_at: "2026-07-31T00:00:00Z",
  version: 1,
};

const NOTICE: ResponseText = {
  message_key: "moira.notice.admin_invite_created",
  message: "The invitation was created.",
};

const BASE_URL = "https://console.example/invite";

function renderModal(secret: string | null) {
  return render(
    <OnceOnlySecretModal
      secret={secret}
      resource={RECORD}
      notice={NOTICE}
      inviteBaseUrl={BASE_URL}
      open
      onDismiss={() => {}}
    />,
  );
}

afterEach(() => {
  cleanup();
  recorded.length = 0;
});

/* -------------------------------------------------------------------------- */

describe("the recorder is actually recording", () => {
  test("rendering the modal calls the wrapped components", () => {
    // Without this, "no recorded prop equals the secret" is true because nothing
    // was recorded — the exact vacuity this file exists to avoid.
    renderModal(TOKEN);
    const names = new Set(recorded.map((entry) => entry.component));
    expect(
      [...names].sort(),
      `recorded: ${JSON.stringify(recorded.map((r) => r.component))}`,
    ).toEqual(["Button", "CopyButton", "Dialog"]);
    expect(recorded.length).toBeGreaterThanOrEqual(4);
  });

  test("the recorder would catch a secret passed as a prop", () => {
    // POSITIVE CONTROL for the recorder itself. If `containsSecret` cannot find
    // a planted value, the assertion below proves nothing.
    const planted: RecordedRender[] = [
      { component: "Fixture", props: { value: TOKEN } },
      { component: "Fixture", props: { nested: { deep: TOKEN } } },
      { component: "Fixture", props: { list: ["a", TOKEN] } },
    ];
    for (const entry of planted) {
      expect(propValuesOf(entry.props)).toContain(TOKEN);
    }
  });
});

/**
 * Every string reachable from a props object, at any depth.
 *
 * `children` is INCLUDED here and excluded by `scalarPropValuesOf` below. The
 * distinction is the one real finding this recorder produced, and it is worth
 * stating precisely rather than tuning away:
 *
 *   A wrapper's `children` is an opaque React element tree that the wrapper
 *   MOUNTS. `<Dialog>` unavoidably receives `<code>{secret}</code>` that way,
 *   because the token has to render inside the dialog — that subtree IS the
 *   modal's own render, merely nested. No API change avoids it short of not
 *   using a dialog.
 *
 *   Every OTHER prop is a value the component READS. `value={secret}`,
 *   `token={secret}`, `title={secret}` are all values a component can log,
 *   serialise into an error, or put in an attribute. That is the class plan
 *   09:324 is about, and `scalarPropValuesOf` is what tests for it.
 *
 * So both are asserted, differently: no component may hold the token in a
 * readable prop, and `Dialog` is the ONLY component allowed to have it anywhere
 * in its props at all.
 */
function propValuesOf(props: Record<string, unknown>): string[] {
  const out: string[] = [];
  const seen = new Set<unknown>();
  const walk = (value: unknown): void => {
    if (typeof value === "string") {
      out.push(value);
      return;
    }
    if (value === null || typeof value !== "object") return;
    if (seen.has(value)) return; // React elements hold circular refs
    seen.add(value);
    if (Array.isArray(value)) {
      for (const item of value) walk(item);
      return;
    }
    for (const item of Object.values(value as Record<string, unknown>)) walk(item);
  };
  walk(props);
  return out;
}

/** The same, minus the top-level `children` element tree. */
function scalarPropValuesOf(props: Record<string, unknown>): string[] {
  const withoutChildren = Object.fromEntries(
    Object.entries(props).filter(([name]) => name !== "children"),
  );
  return propValuesOf(withoutChildren);
}

describe("the token is handed to no other component", () => {
  test("no READABLE prop of any rendered component carries the token", () => {
    renderModal(TOKEN);

    const offenders = recorded
      .filter((entry) => scalarPropValuesOf(entry.props).some((value) => value.includes(TOKEN)))
      .map((entry) => entry.component);

    expect(
      offenders,
      "plan 09:324 — the token appears exactly once, in this modal, and never as a prop on a " +
        "reusable component beyond its own render. `CopyButton` takes an element id for this " +
        "reason.",
    ).toEqual([]);
  });

  test("the composed invite URL is not handed to another component either", () => {
    // The URL embeds the token, so a caller-built link would be a SECOND holder.
    // This is why the modal composes it inline instead of taking it as a prop.
    renderModal(TOKEN);
    const url = `${BASE_URL}/${encodeURIComponent(TOKEN)}`;
    const offenders = recorded
      .filter((entry) => scalarPropValuesOf(entry.props).some((value) => value.includes(url)))
      .map((entry) => entry.component);
    expect(offenders).toEqual([]);
  });

  test("Dialog is the ONLY component whose props contain the token at all", () => {
    // Including `children`. `<Dialog>` gets the token inside the element tree it
    // mounts and there is no way around that; every other component must not see
    // it even that way, which is what would break if the `<code>` moved inside
    // `CopyButton` or a future `SecretField` molecule.
    renderModal(TOKEN);
    const holders = [
      ...new Set(
        recorded
          .filter((entry) => propValuesOf(entry.props).some((value) => value.includes(TOKEN)))
          .map((entry) => entry.component),
      ),
    ].sort();
    expect(holders).toEqual(["Dialog"]);
  });

  test("CopyButton receives an element id, never a value", () => {
    renderModal(TOKEN);
    const copyProps = recorded.filter((entry) => entry.component === "CopyButton");
    expect(copyProps.length).toBe(2);
    for (const entry of copyProps) {
      expect(Object.keys(entry.props).sort()).toEqual(["aria-describedby", "targetId"]);
      expect(typeof entry.props["targetId"]).toBe("string");
    }
  });
});

describe("the token is rendered exactly once, in its own element", () => {
  test("one element holds the raw token", () => {
    const { container } = renderModal(TOKEN);
    const holders = [...container.querySelectorAll("*")].filter(
      (node) => node.childElementCount === 0 && node.textContent === TOKEN,
    );
    expect(holders, "the raw token should be the text of exactly one leaf element").toHaveLength(1);
    expect(holders[0]!.tagName.toLowerCase()).toBe("code");
    expect(holders[0]!.id).not.toBe("");
  });

  test("the link element holds the token exactly once too", () => {
    const { container } = renderModal(TOKEN);
    const url = `${BASE_URL}/${encodeURIComponent(TOKEN)}`;
    const holders = [...container.querySelectorAll("code")].filter(
      (node) => node.textContent === url,
    );
    expect(holders).toHaveLength(1);
  });

  test("no attribute anywhere carries the token", () => {
    // A `title`, a `data-*`, or an `aria-label` built from the value would put it
    // somewhere the copy control does not need it to be.
    const { container } = renderModal(TOKEN);
    const offenders: string[] = [];
    for (const node of container.querySelectorAll("*")) {
      for (const attribute of node.attributes) {
        if (attribute.value.includes(TOKEN)) offenders.push(`${node.tagName}[${attribute.name}]`);
      }
    }
    expect(offenders).toEqual([]);
  });
});

describe("`secret === null` is the normal idempotent-replay case", () => {
  test("it renders an explanation, not a failure", () => {
    renderModal(null);
    expect(screen.getByRole("status")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.secret_already_shown),
    );
    // Not an alert: nothing went wrong.
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  test("it offers no copy control and renders no code block", () => {
    const { container } = renderModal(null);
    expect(recorded.filter((entry) => entry.component === "CopyButton")).toEqual([]);
    expect(container.querySelectorAll("code")).toHaveLength(0);
  });

  test("Moira's notice and the expiry still render", () => {
    renderModal(null);
    expect(screen.getByText(NOTICE.message)).toBeInTheDocument();
    expect(
      screen.getByText(
        copy(CONSOLE_MESSAGE_KEYS.secret_expires_at).replace("{expires_at}", RECORD.expires_at),
      ),
    ).toBeInTheDocument();
  });
});

describe("the notice goes through t(), never as hardcoded English", () => {
  test("an uncatalogued Moira key falls back to the server's message", () => {
    renderModal(TOKEN);
    expect(screen.getByText(NOTICE.message)).toBeInTheDocument();
  });

  test("the warning is an alert and comes from the catalog", () => {
    renderModal(TOKEN);
    expect(screen.getByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.secret_shown_once),
    );
  });

  test("the dialog is named from the catalog", () => {
    renderModal(TOKEN);
    const dialogProps = recorded.find((entry) => entry.component === "Dialog");
    expect(dialogProps!.props["label"]).toBe(copy(CONSOLE_MESSAGE_KEYS.secret_modal_heading));
  });
});
