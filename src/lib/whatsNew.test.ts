import { describe, expect, it } from "vitest";
import { unseenEntries, type WhatsNewEntry } from "./whatsNew";

const entries: WhatsNewEntry[] = [
  { id: "c", title: "C", description: "" },
  { id: "b", title: "B", description: "" },
  { id: "a", title: "A", description: "" },
];

describe("unseenEntries", () => {
  it("shows everything the first time", () => {
    expect(unseenEntries(null, entries)).toEqual(entries);
  });

  it("shows only entries newer than the last one seen", () => {
    expect(unseenEntries("b", entries).map((e) => e.id)).toEqual(["c"]);
  });

  it("shows nothing once the newest has been seen", () => {
    expect(unseenEntries("c", entries)).toEqual([]);
  });

  it("shows everything when the seen entry was removed", () => {
    expect(unseenEntries("gone", entries)).toEqual(entries);
  });
});
