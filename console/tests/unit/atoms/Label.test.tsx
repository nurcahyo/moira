import { describe, expect, it } from "bun:test";
import { render, screen } from "@testing-library/react";
import { Label } from "@/components/atoms/Label";

describe("Label", () => {
  it("associates with its control via htmlFor/id so the input gets an accessible name", () => {
    render(
      <>
        <Label htmlFor="email-field">Email</Label>
        <input id="email-field" />
      </>,
    );
    expect(screen.getByRole("textbox", { name: "Email" })).toBeInTheDocument();
  });

  it("renders a visible '*' plus a screen-reader-only '(required)' when required", () => {
    render(
      <Label htmlFor="name-field" required>
        Name
      </Label>,
    );
    const label = screen.getByText("Name", { exact: false }).closest("label");
    expect(label).not.toBeNull();
    expect(label).toHaveTextContent("*");
    expect(label).toHaveTextContent("(required)");
  });

  it("renders no required indicator by default", () => {
    render(<Label htmlFor="name-field">Name</Label>);
    const label = screen.getByText("Name").closest("label");
    expect(label).not.toHaveTextContent("*");
    expect(label).not.toHaveTextContent("required");
  });
});
