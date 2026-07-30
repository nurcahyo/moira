import { describe, expect, it } from "bun:test";
import { render, screen } from "@testing-library/react";
import { Spinner } from "@/components/atoms/Spinner";

describe("Spinner", () => {
  it("exposes role=status with a default 'Loading' accessible name", () => {
    render(<Spinner />);
    expect(screen.getByRole("status")).toHaveAccessibleName("Loading");
  });

  it("is a polite live region, not assertive, so it doesn't interrupt", () => {
    render(<Spinner />);
    expect(screen.getByRole("status")).toHaveAttribute("aria-live", "polite");
  });

  it("uses a custom accessible label when provided", () => {
    render(<Spinner label="Saving changes" />);
    expect(screen.getByRole("status")).toHaveAccessibleName("Saving changes");
  });

  it("hides the decorative animated element from assistive tech", () => {
    render(<Spinner />);
    const status = screen.getByRole("status");
    const decorative = status.querySelector("[aria-hidden='true']");
    expect(decorative).not.toBeNull();
  });
});
