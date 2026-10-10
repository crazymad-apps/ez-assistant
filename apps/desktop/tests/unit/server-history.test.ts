import { describe, expect, it } from "vitest";
import { serverHistorySupport, type ProviderConnection } from "@ez-assistant/protocol";
import cases from "../../../../packages/assistant-protocol/fixtures/server-history.json";

describe("server history shared contract", () => {
  for (const fixture of cases) {
    it(fixture.name, () => {
      expect(serverHistorySupport(fixture.connection as ProviderConnection)).toBe(fixture.expected);
    });
  }
});
