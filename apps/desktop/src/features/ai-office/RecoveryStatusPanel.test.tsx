import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { RecoveryStatusPanel, type RecoveryObservation, type RecoveryState } from "./RecoveryStatusPanel";

const observation: RecoveryObservation = {
  fallback_id: "fallback-17",
  root_task_id: "root-8",
  source_task_id: "task-12",
  source_attempt_id: "attempt-2",
  source_job_id: "job-31",
  fallback_agent_id: "agent-9",
  policy_revision: 7,
  reason_code: "execution_timeout",
  state: "running",
  blocked_reason: null,
  revision: 4,
};

describe("RecoveryStatusPanel", () => {
  it("defaults to disconnected and never presents supplied stale work as live running", () => {
    render(<RecoveryStatusPanel observation={observation} slotOccupancy={1} />);
    expect(screen.getByText("恢复观测未接入")).toBeInTheDocument();
    expect(screen.getByLabelText("恢复槽占用未核实，上限 1")).toBeInTheDocument();
    expect(screen.queryByText("正在救场")).not.toBeInTheDocument();
    expect(screen.queryByText("job-31")).not.toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("makes a missing GPT host explicit without inventing a running companion", () => {
    render(<RecoveryStatusPanel availability="host_unconfigured" slotOccupancy={0} />);
    expect(screen.getByText("等待配置 GPT 宿主")).toBeInTheDocument();
    expect(screen.getByText(/GPT 创建、查询和停止接口尚未配置/)).toBeInTheDocument();
    expect(screen.getByLabelText("恢复槽占用 0，上限 1")).toBeInTheDocument();
    expect(screen.queryByText("正在救场")).not.toBeInTheDocument();
  });

  it("shows verified slot occupancy separately from ordinary team limits and preserves task linkage", () => {
    render(<RecoveryStatusPanel observation={observation} availability="connected" slotOccupancy={1} nickname="奶盖" />);
    expect(screen.getByText("奶盖")).toBeInTheDocument();
    expect(screen.getByLabelText("恢复槽占用 1，上限 1")).toBeInTheDocument();
    expect(screen.getByText(/不计入常规子 Agent 人数上下限/)).toBeInTheDocument();
    for (const value of ["fallback-17", "root-8", "task-12", "attempt-2", "job-31", "agent-9", "execution_timeout"]) {
      expect(screen.getByText(value)).toBeInTheDocument();
    }
    expect(screen.getByText("策略 v7 · 记录 v4")).toBeInTheDocument();
    expect(screen.getByText("原 CLI Job 的状态需独立查看。")).toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  });

  it("does not derive a slot count from a running or blocked state", () => {
    const { rerender } = render(<RecoveryStatusPanel observation={observation} availability="connected" />);
    expect(screen.getByLabelText("恢复槽占用未核实，上限 1")).toBeInTheDocument();
    rerender(<RecoveryStatusPanel observation={{ ...observation, state: "blocked", blocked_reason: "等待宿主档位选择" }} availability="connected" />);
    expect(screen.getByLabelText("恢复槽占用未核实，上限 1")).toBeInTheDocument();
    expect(screen.getByText("等待宿主档位选择")).toBeInTheDocument();
  });

  it.each<[RecoveryState, string]>([
    ["waiting_source_stop", "等待原 CLI 停止确认"],
    ["queued", "等待救场"],
    ["starting", "救场伙伴启动中"],
    ["running", "正在救场"],
    ["succeeded", "恢复已验收"],
    ["failed", "恢复未成功"],
    ["blocked", "恢复受阻"],
    ["cancelled", "恢复已取消"],
    ["unknown", "恢复结果待核实"],
    ["confirmed_not_started", "已核实未启动"],
    ["verified_terminal", "宿主终态已核实"],
  ])("renders the observed %s state without claiming a percentage", (state, label) => {
    render(<RecoveryStatusPanel observation={{ ...observation, state }} availability="connected" />);
    expect(within(screen.getByRole("status")).getByText(label)).toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  });

  it("keeps unknown outcomes uncertain without retry or start actions", () => {
    render(<RecoveryStatusPanel observation={{ ...observation, state: "unknown", blocked_reason: "发送结果待核对" }} availability="connected" slotOccupancy={1} />);
    expect(screen.getByText(/不能据此判断可以重新发起任务/)).toBeInTheDocument();
    expect(screen.getByText("发送结果待核对")).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    expect(screen.queryByText(/安全重试/)).not.toBeInTheDocument();
  });

  it("marks supplied sample data and represents absent job/agent identities honestly", () => {
    render(<RecoveryStatusPanel observation={{ ...observation, source_job_id: null, fallback_agent_id: null }} availability="connected" source="sample" slotOccupancy={1} />);
    expect(screen.getByText("SAMPLE · 样式样例")).toBeInTheDocument();
    expect(screen.getByText("以下为展示样例，没有启动真实恢复任务。")).toBeInTheDocument();
    expect(screen.getByText("尚未关联 Job")).toBeInTheDocument();
    expect(screen.getByText("尚未关联成员")).toBeInTheDocument();
  });

  it("renders an empty connected observation without creating a fallback record", () => {
    render(<RecoveryStatusPanel availability="connected" slotOccupancy={0} />);
    expect(screen.getByText("当前没有恢复记录")).toBeInTheDocument();
    expect(screen.getByLabelText("恢复槽占用 0，上限 1")).toBeInTheDocument();
    expect(screen.queryByText("恢复记录")).not.toBeInTheDocument();
  });
});
