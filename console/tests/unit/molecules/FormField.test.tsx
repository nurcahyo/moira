import { describe, expect, it } from "bun:test";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FormField } from "@/components/molecules/FormField";

describe("FormField", () => {
  it("composes Label + Input so the label gives the input its accessible name", () => {
    render(<FormField label="Email" />);
    expect(
      screen.getByRole("textbox", { name: "Email" }),
    ).toBeInTheDocument();
  });

  it("marks the field required with a visible and screen-reader-accessible indicator", () => {
    render(<FormField label="Email" required />);
    const label = screen.getByText("Email", { exact: false }).closest(
      "label",
    );
    expect(label).toHaveTextContent("(required)");
    expect(
      screen.getByRole("textbox", { name: "Email" }),
    ).toBeRequired();
  });

  it("shows hint text and wires it via aria-describedby when there is no error", () => {
    render(<FormField label="Email" hint="We never share this." />);
    const input = screen.getByRole("textbox", { name: "Email" });
    expect(input).toHaveAccessibleDescription("We never share this.");
    expect(input).not.toHaveAttribute("aria-invalid");
  });

  it("shows error text instead of hint, marks invalid, and announces via role=alert", () => {
    render(
      <FormField
        label="Email"
        hint="We never share this."
        error="Enter a valid email address."
      />,
    );
    const input = screen.getByRole("textbox", { name: "Email" });
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(input).toHaveAccessibleDescription(
      "Enter a valid email address.",
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Enter a valid email address.",
    );
    expect(
      screen.queryByText("We never share this."),
    ).not.toBeInTheDocument();
  });

  it("forwards arbitrary inputProps (e.g. placeholder) to the underlying Input", async () => {
    render(
      <FormField
        label="Email"
        inputProps={{ placeholder: "you@example.com" }}
      />,
    );
    const input = screen.getByRole("textbox", { name: "Email" });
    expect(input).toHaveAttribute("placeholder", "you@example.com");
    await userEvent.type(input, "a");
    expect(input).toHaveValue("a");
  });

  it("generates a stable id and correctly associates label + input when none is given", () => {
    render(<FormField label="Email" />);
    const input = screen.getByRole("textbox", { name: "Email" });
    const label = screen.getByText("Email").closest("label");
    expect(label).not.toBeNull();
    expect(label?.getAttribute("for")).toBe(input.getAttribute("id"));
  });
});
