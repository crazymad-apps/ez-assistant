import { describe, expect, it } from "vitest";
import { checkCompatibility, currentCompatibility, compatibilityHeaders } from "@ez-assistant/protocol";
import vectors from "@ez-assistant/protocol/fixtures/compatibility.json";

describe("shared software compatibility", () => {
  for (const vector of vectors) {
    it(vector.name, () => {
      expect(checkCompatibility(vector.client, vector.host)?.code ?? null).toBe(vector.error);
    });
  }
  it("uses the page build and strips invalid diagnostics", () => {
    const own = currentCompatibility();
    expect(compatibilityHeaders()["x-ez-client-version"]).toBe(own.version);
    expect(checkCompatibility({ version: "secret", min_compatible_version: "0.25.2" }, own)?.client).toBeNull();
    expect(checkCompatibility(own, { version: "0.25.1" })?.code).toBe("invalid_declaration");
  });
});
