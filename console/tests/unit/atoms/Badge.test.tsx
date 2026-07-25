import { describe, expect, it } from "bun:test";
import { render, screen } from "@testing-library/react";
import { Badge } from "@/components/atoms/Badge";

describe("Badge", () => {
  it("always renders its text content (color is never the only signal)", () => {
    render(<Badge tone="danger">Failed</Badge>);
    expect(screen.getByText("Failed")).toBeInTheDocument();
  });

  it("has no ARIA role by default (static label)", () => {
    render(<Badge>Draft</Badge>);
    expect(screen.getByText("Draft")).not.toHaveAttribute("role");
  });

  it("exposes role=status and is announced as a live region when live=true", () => {
    render(<Badge live>Saved</Badge>);
    expect(screen.getByRole("status")).toHaveTextContent("Saved");
  });

  it("passes through arbitrary HTML span attributes", () => {
    render(
      <Badge data-testid="tier-badge" title="Tier: gold">
        Gold
      </Badge>,
    );
    expect(screen.getByTestId("tier-badge")).toHaveAttribute(
      "title",
      "Tier: gold",
    );
  });
});
