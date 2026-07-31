// The destructive-confirmation molecule.

import { describe, expect, test } from "bun:test";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { DangerConfirmDialog } from "@/components/molecules/DangerConfirmDialog";
import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

function dialog(overrides: Partial<Parameters<typeof DangerConfirmDialog>[0]> = {}) {
  return (
    <DangerConfirmDialog
      open
      title="Revoke admin access?"
      body="They lose access immediately."
      confirmLabel="Revoke access"
      onConfirm={() => {}}
      onCancel={() => {}}
      {...overrides}
    />
  );
}

describe("it composes the Dialog atom rather than reimplementing it", () => {
  test("the accessible name is the title, so it is not announced as 'dialog'", () => {
    render(dialog());
    expect(screen.getByRole("dialog", { name: "Revoke admin access?" })).toBeDefined();
  });

  test("the consequence is announced, not left to be found", () => {
    render(dialog());
    expect(screen.getByRole("alert").textContent).toBe("They lose access immediately.");
  });
});

describe("the destructive control is never the one a reflexive Enter presses", () => {
  test("cancel comes FIRST in DOM order", () => {
    // `showModal()` focuses the first tabbable descendant, so DOM order decides
    // which control a keyboard user activates by pressing Enter to dismiss.
    // Putting the destructive one there would make "get rid of this dialog"
    // perform the action.
    render(dialog());
    const buttons = screen.getAllByRole("button").map((button) => button.textContent);
    expect(buttons[0]).toBe(copy(CONSOLE_MESSAGE_KEYS.action_cancel));
    expect(buttons[1]).toBe("Revoke access");
  });

  test("cancel's label is a catalog key, because it is the one string this file owns", () => {
    render(dialog());
    expect(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.action_cancel) }),
    ).toBeDefined();
  });
});

describe("behaviour", () => {
  test("confirming and cancelling call the right callback, once", async () => {
    let confirmed = 0;
    let cancelled = 0;
    render(dialog({ onConfirm: () => (confirmed += 1), onCancel: () => (cancelled += 1) }));

    await userEvent.click(screen.getByRole("button", { name: "Revoke access" }));
    expect(confirmed).toBe(1);
    expect(cancelled).toBe(0);

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.action_cancel) }),
    );
    expect(cancelled).toBe(1);
    expect(confirmed).toBe(1);
  });

  test("while busy, both controls are unavailable", () => {
    render(dialog({ busy: true }));
    for (const button of screen.getAllByRole("button")) {
      expect(button.hasAttribute("disabled")).toBe(true);
    }
    // And the destructive one says WHY it is unavailable to assistive tech.
    expect(screen.getByRole("button", { name: "Revoke access" }).getAttribute("aria-busy")).toBe(
      "true",
    );
  });

  test("closed means not rendered open", () => {
    render(dialog({ open: false }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
