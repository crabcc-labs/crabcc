import { afterEach, describe, expect, it } from "bun:test";
import { render } from "../../../test/render";
import { DashTile } from "./DashTile";

const cleanups: Array<() => void> = [];
afterEach(() => {
  while (cleanups.length) cleanups.pop()!();
});

describe("<DashTile />", () => {
  it("renders title + body content", () => {
    const r = render(<DashTile title="hello"><span>body</span></DashTile>);
    cleanups.push(r.unmount);
    expect(r.container.querySelector(".dash-tile-title")?.textContent).toBe("hello");
    expect(r.container.textContent).toContain("body");
  });

  it("renders an open link only when openHref is set", () => {
    const r = render(
      <DashTile title="t" openHref="#/logs" openLabel="open logs">
        body
      </DashTile>,
    );
    cleanups.push(r.unmount);
    const link = r.container.querySelector(".dash-tile-open") as HTMLAnchorElement;
    expect(link).not.toBeNull();
    expect(link.getAttribute("href")).toBe("#/logs");
    expect(link.textContent).toContain("open logs");
  });

  it("omits the open link when openHref is unset", () => {
    const r = render(<DashTile title="t">body</DashTile>);
    cleanups.push(r.unmount);
    expect(r.container.querySelector(".dash-tile-open")).toBeNull();
  });

  it("applies a grid-area when `area` is set", () => {
    const r = render(<DashTile title="t" area="kpi-live">body</DashTile>);
    cleanups.push(r.unmount);
    const tile = r.container.querySelector(".dash-tile") as HTMLElement;
    expect(tile.style.gridArea).toBe("kpi-live");
  });

  it("renders a meta chip when `meta` is set", () => {
    const r = render(
      <DashTile title="t" meta={<span data-testid="m">x</span>}>body</DashTile>,
    );
    cleanups.push(r.unmount);
    expect(r.container.querySelector(".dash-tile-meta [data-testid=m]")).not.toBeNull();
  });
});
