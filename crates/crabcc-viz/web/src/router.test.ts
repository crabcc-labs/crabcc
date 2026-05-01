// Pure unit tests for the hash router. No DOM needed.

import { describe, expect, it } from "bun:test";
import { routeFor } from "./router";

describe("routeFor", () => {
  it("maps the empty / root hash to the dashboard", () => {
    expect(routeFor("")).toBe("dashboard");
    expect(routeFor("#")).toBe("dashboard");
    expect(routeFor("#/")).toBe("dashboard");
  });

  it("maps #/knowledge (and #knowledge) to the knowledge view", () => {
    expect(routeFor("#/knowledge")).toBe("knowledge");
    expect(routeFor("#knowledge")).toBe("knowledge");
  });

  it("falls back to the dashboard for unknown routes", () => {
    expect(routeFor("#/whatever")).toBe("dashboard");
  });
});
