import { describe, expect, it } from "bun:test";
import { render, screen } from "@testing-library/react";
import { StatusBadgeGroup } from "@/components/molecules/StatusBadgeGroup";

describe("StatusBadgeGroup", () => {
  it("renders one badge per item inside a labeled list", () => {
    render(
      <StatusBadgeGroup
        aria-label="Provider status"
        items={[
          { key: "openai", label: "OpenAI: healthy", tone: "success" },
          { key: "azure", label: "Azure: degraded", tone: "warning" },
        ]}
      />,
    );
    const list = screen.getByRole("list", { name: "Provider status" });
    expect(list).toBeInTheDocument();
    expect(screen.getByText("OpenAI: healthy")).toBeInTheDocument();
    expect(screen.getByText("Azure: degraded")).toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
  });

  it("renders nothing (no empty list landmark) when there are no items", () => {
    render(<StatusBadgeGroup aria-label="Provider status" items={[]} />);
    expect(
      screen.queryByRole("list", { name: "Provider status" }),
    ).not.toBeInTheDocument();
  });

  it("preserves item order", () => {
    render(
      <StatusBadgeGroup
        aria-label="Steps"
        items={[
          { key: "a", label: "First" },
          { key: "b", label: "Second" },
          { key: "c", label: "Third" },
        ]}
      />,
    );
    const items = screen.getAllByRole("listitem").map((el) => el.textContent);
    expect(items).toEqual(["First", "Second", "Third"]);
  });
});
