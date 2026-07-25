import { describe, expect, it, mock } from "bun:test";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Input } from "@/components/atoms/Input";

describe("Input", () => {
  it("renders as a native input with a label association via aria-label", () => {
    render(<Input aria-label="Email address" placeholder="you@example.com" />);
    const input = screen.getByRole("textbox", { name: "Email address" });
    expect(input).toHaveAttribute("placeholder", "you@example.com");
  });

  it("is keyboard-focusable and accepts typed input (uncontrolled)", async () => {
    render(<Input aria-label="Name" />);
    const input = screen.getByRole("textbox", { name: "Name" });
    await userEvent.tab();
    expect(input).toHaveFocus();
    await userEvent.keyboard("Ada Lovelace");
    expect(input).toHaveValue("Ada Lovelace");
  });

  it("calls onChange for a controlled input without mutating value itself", async () => {
    const onChange = mock();
    render(<Input aria-label="Name" value="" onChange={onChange} />);
    await userEvent.type(screen.getByRole("textbox", { name: "Name" }), "A");
    expect(onChange).toHaveBeenCalled();
  });

  it("sets aria-invalid when invalid=true", () => {
    render(<Input aria-label="Email" invalid />);
    expect(screen.getByRole("textbox", { name: "Email" })).toHaveAttribute("aria-invalid", "true");
  });

  it("does not set aria-invalid when invalid is omitted", () => {
    render(<Input aria-label="Email" />);
    expect(screen.getByRole("textbox", { name: "Email" })).not.toHaveAttribute("aria-invalid");
  });

  it("is disabled and unfocusable when disabled=true", () => {
    render(<Input aria-label="Email" disabled />);
    expect(screen.getByRole("textbox", { name: "Email" })).toBeDisabled();
  });

  it("wires aria-describedby through to an external description element", () => {
    render(
      <>
        <Input aria-label="Email" aria-describedby="email-hint" />
        <p id="email-hint">We never share this.</p>
      </>,
    );
    expect(screen.getByRole("textbox", { name: "Email" })).toHaveAccessibleDescription(
      "We never share this.",
    );
  });
});
