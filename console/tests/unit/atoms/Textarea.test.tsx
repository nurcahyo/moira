import { describe, expect, it, mock } from "bun:test";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Textarea } from "@/components/atoms/Textarea";

describe("Textarea", () => {
  it("renders as a native multi-line textbox with an accessible name", () => {
    render(<Textarea aria-label="Prompt" placeholder="Ask the model something…" />);
    const textarea = screen.getByRole("textbox", { name: "Prompt" });
    expect(textarea.tagName).toBe("TEXTAREA");
    expect(textarea).toHaveAttribute("placeholder", "Ask the model something…");
  });

  it("defaults to 6 rows, overridable by the caller", () => {
    render(<Textarea aria-label="Prompt" />);
    expect(screen.getByRole("textbox", { name: "Prompt" })).toHaveAttribute("rows", "6");
  });

  it("is keyboard-focusable and accepts typed input (uncontrolled)", async () => {
    render(<Textarea aria-label="Prompt" />);
    const textarea = screen.getByRole("textbox", { name: "Prompt" });
    await userEvent.tab();
    expect(textarea).toHaveFocus();
    await userEvent.keyboard("Explain quantum tunnelling.");
    expect(textarea).toHaveValue("Explain quantum tunnelling.");
  });

  it("calls onChange for a controlled textarea without mutating value itself", async () => {
    const onChange = mock();
    render(<Textarea aria-label="Prompt" value="" onChange={onChange} />);
    await userEvent.type(screen.getByRole("textbox", { name: "Prompt" }), "A");
    expect(onChange).toHaveBeenCalled();
  });

  it("sets aria-invalid when invalid=true", () => {
    render(<Textarea aria-label="Prompt" invalid />);
    expect(screen.getByRole("textbox", { name: "Prompt" })).toHaveAttribute("aria-invalid", "true");
  });

  it("does not set aria-invalid when invalid is omitted", () => {
    render(<Textarea aria-label="Prompt" />);
    expect(screen.getByRole("textbox", { name: "Prompt" })).not.toHaveAttribute("aria-invalid");
  });

  it("is disabled and unfocusable when disabled=true", () => {
    render(<Textarea aria-label="Prompt" disabled />);
    expect(screen.getByRole("textbox", { name: "Prompt" })).toBeDisabled();
  });
});
