import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { OfficeGlyph, OfficeScene, type SceneMember } from "./OfficeScene";

const members: SceneMember[] = [
  { id: "rice", name: "糯米", role: "总经理", state: "工作中", task: "检查交付", parentId: null, working: true, kind: "manager" },
  { id: "peach", name: "桃桃", role: "小伙伴", state: "待命中", task: "暂无任务", parentId: "rice", working: false, kind: "agent" },
];
afterEach(() => vi.unstubAllGlobals());

describe("walking office", () => {
  it("moves named characters separately from furniture and keeps idle member selection available", () => {
    const onSelect = vi.fn();
    render(<OfficeScene members={members} selectedId="rice" onSelect={onSelect} demo />);
    const rice = screen.getByRole("button", { name: "查看糯米的任务" });
    const peach = screen.getByRole("button", { name: "查看桃桃的任务" });
    expect(rice).toHaveClass("ao-wanderer", "is-selected");
    expect(rice.style.getPropertyValue("--ao-home-x")).toBe("25%");
    expect(rice.style.getPropertyValue("--ao-stop-x")).toBe("90%");
    expect(rice.style.getPropertyValue("--ao-home-y")).not.toBe(rice.style.getPropertyValue("--ao-lane-y"));
    expect(peach.style.getPropertyValue("--ao-delay")).not.toBe(rice.style.getPropertyValue("--ao-delay"));
    expect(screen.getByTestId("fixed-desk-rice")).not.toHaveClass("ao-wanderer");
    expect(peach).not.toHaveClass("is-working");
    fireEvent.click(peach);
    expect(onSelect).toHaveBeenCalledWith("peach");
    expect(screen.getByText(/走动是装饰动效，不代表实际任务行为/)).toBeInTheDocument();
  });

  it("pauses and resumes the whole scene without changing real member state", () => {
    render(<OfficeScene members={members} selectedId={null} onSelect={vi.fn()} demo={false} />);
    fireEvent.click(screen.getByRole("button", { name: "Ⅱ 暂停走动" }));
    expect(screen.getByTestId("office-scene")).toHaveClass("is-paused");
    expect(screen.getByRole("button", { name: "▶ 继续走动" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "查看糯米的任务" })).toHaveClass("is-working");
    fireEvent.click(screen.getByRole("button", { name: "▶ 继续走动" }));
    expect(screen.getByTestId("office-scene")).not.toHaveClass("is-paused");
  });

  it("respects the system reduced-motion preference", () => {
    vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() })));
    render(<OfficeScene members={members} selectedId={null} onSelect={vi.fn()} demo />);
    expect(screen.getByTestId("office-scene")).toHaveClass("is-paused");
    expect(screen.getByRole("button", { name: "系统已减少动态" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "查看桃桃的任务" })).toBeEnabled();
  });

  it("uses the same atlas character in the office and member profile", () => {
    render(<><OfficeScene members={members} selectedId="rice" onSelect={vi.fn()} demo /><div data-testid="profile"><OfficeGlyph kind="manager" residentId="rice" /></div></>);
    const person = screen.getByRole("button", { name: "查看糯米的任务" });
    const officeSprite = person.querySelector("[data-resident-index]");
    const profileSprite = screen.getByTestId("profile").querySelector("[data-resident-index]");
    expect(officeSprite?.getAttribute("data-resident-index")).toBe(profileSprite?.getAttribute("data-resident-index"));
    expect(person.querySelector("svg")).toBeNull();
    expect(person.querySelector(".ao-resident-standing")).toBeInTheDocument();
    expect(person.querySelector(".ao-resident-stride")).toBeInTheDocument();
    expect(profileSprite?.querySelector(".ao-resident-walking")).toBeNull();
  });
});
