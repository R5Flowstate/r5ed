import { describe, expect, it } from "vitest";
import { render } from "solid-js/web";
import { DEV_MATERIALS } from "../doc/model";
import { MaterialField, groupOptions, materialOptions } from "./MaterialPicker";

describe("material picker list", () => {
  it("leads with the dev set, in the order the maps name it", () => {
    const options = materialOptions("world/dev/dev_gray_512", []);
    expect(options.slice(0, DEV_MATERIALS.length)).toEqual(DEV_MATERIALS.map((value) => ({ value, group: "dev" })));
  });

  it("puts the map's own materials after the dev set, sorted and deduplicated", () => {
    const options = materialOptions("", ["world/props/rock", "world/dev/dev_blue_512", "world/props/rock", "world/props/beam"]);
    const project = groupOptions(options, "project").map((o) => o.value);
    expect(project).toEqual(["world/props/beam", "world/props/rock"]);
    const devCount = groupOptions(options, "dev").length;
    expect(options.slice(devCount, devCount + 2).every((o) => o.group === "project")).toBe(true);
    expect(options.some((o) => o.group === "dev" && o.value === "world/dev/dev_blue_512")).toBe(true);
    expect(options.filter((o) => o.value === "world/dev/dev_blue_512")).toHaveLength(1);
  });

  it("never drops the value in hand, even one nothing else lists", () => {
    const options = materialOptions("world/hand_typed/thing", []);
    expect(options[options.length - 1]).toEqual({ value: "world/hand_typed/thing", group: "current" });
  });

  it("does not repeat a value in hand that is already listed", () => {
    const options = materialOptions("world/dev/dev_gray_512", ["world/props/rock"]);
    expect(options.filter((o) => o.value === "world/dev/dev_gray_512")).toHaveLength(1);
    expect(groupOptions(options, "current")).toEqual([]);
  });
});

describe("material picker widget", () => {
  function mount(value: string, project: string[]): { root: HTMLDivElement; chosen: string[]; dispose: () => void } {
    const root = document.createElement("div");
    document.body.appendChild(root);
    const chosen: string[] = [];
    const dispose = render(
      () =>
        MaterialField({
          label: "Material",
          value,
          project,
          onChange: (v) => chosen.push(v),
        }),
      root,
    );
    return { root, chosen, dispose };
  }

  it("shows the groups in the order the list is built", () => {
    const { root, dispose } = mount("world/dev/dev_gray_512", ["world/props/rock"]);
    const groups = [...root.querySelectorAll("optgroup")].map((g) => g.getAttribute("label"));
    expect(groups).toEqual(["Dev materials", "Map materials"]);
    const select = root.querySelector("select");
    expect(select?.getAttribute("data-material-options")).toBe(String(DEV_MATERIALS.length + 1));
    dispose();
    root.remove();
  });

  it("preselects the value and reports the one chosen", () => {
    const { root, chosen, dispose } = mount("world/dev/dev_red_512", []);
    const select = root.querySelector<HTMLSelectElement>("select");
    if (!select) throw new Error("no select");
    expect(select.value).toBe("world/dev/dev_red_512");
    select.value = "world/dev/dev_blue_512";
    select.dispatchEvent(new Event("change"));
    expect(chosen).toEqual(["world/dev/dev_blue_512"]);
    dispose();
    root.remove();
  });

  it("omits the map group when the document names no material of its own", () => {
    const { root, dispose } = mount("world/dev/dev_gray_512", []);
    const groups = [...root.querySelectorAll("optgroup")].map((g) => g.getAttribute("label"));
    expect(groups).toEqual(["Dev materials"]);
    dispose();
    root.remove();
  });
});
