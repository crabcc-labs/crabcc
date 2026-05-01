import { afterEach, describe, expect, it } from "bun:test";
import type { TelemetryEvent } from "../../api";
import { render } from "../../../test/render";
import { LogsView } from "./LogsView";

const cleanups: Array<() => void> = [];
afterEach(() => {
  while (cleanups.length) cleanups.pop()!();
});

const now = Math.floor(Date.now() / 1000);

function ev(level: TelemetryEvent["level"], target: string, msg = "msg"): TelemetryEvent {
  return { ts: now, level, target, fields: { message: msg } };
}

describe("<LogsView />", () => {
  it("renders an empty state when there are no events", () => {
    const r = render(<LogsView events={[]} />);
    cleanups.push(r.unmount);
    expect(r.container.querySelector(".logs-empty")).not.toBeNull();
    expect(r.container.textContent).toContain("0");
  });

  it("renders the level breakdown pills with counts", () => {
    const r = render(
      <LogsView
        events={[ev("INFO", "x"), ev("INFO", "y"), ev("ERROR", "z")]}
      />,
    );
    cleanups.push(r.unmount);
    const pills = r.container.querySelectorAll(".logs-level-pill");
    expect(pills.length).toBe(5);
    const infoPill = r.container.querySelector(".logs-level-INFO") as HTMLElement;
    expect(infoPill.textContent).toContain("INFO");
    expect(infoPill.textContent).toContain("2");
  });

  it("renders one row per event", () => {
    const r = render(<LogsView events={[ev("INFO", "x"), ev("WARN", "y")]} />);
    cleanups.push(r.unmount);
    expect(r.container.querySelectorAll(".logs-row").length).toBe(2);
  });
});
