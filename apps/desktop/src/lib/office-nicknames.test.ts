import { afterEach, describe, expect, it, vi } from "vitest";
import { nicknameForTask } from "./office-nicknames";

afterEach(() => { vi.restoreAllMocks(); });

describe("real task display nicknames", () => {
  it("keeps a stable nickname for the same real task ID", () => {
    const id = "wc_agent_task_0123456789abcdef0123456789abcdef";
    expect(nicknameForTask(id)).toBe(nicknameForTask(id));
    expect(nicknameForTask(id).length).toBeGreaterThan(0);
    expect(nicknameForTask("child-a")).toBe("糯米");
  });

  it("does not depend on random choices or browser storage", () => {
    const random = vi.spyOn(Math, "random");
    const readStorage = vi.spyOn(Storage.prototype, "getItem");
    const writeStorage = vi.spyOn(Storage.prototype, "setItem");
    nicknameForTask("real-project-task");
    expect(random).not.toHaveBeenCalled();
    expect(readStorage).not.toHaveBeenCalled();
    expect(writeStorage).not.toHaveBeenCalled();
  });
});
