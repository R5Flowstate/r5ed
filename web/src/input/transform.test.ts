import { describe, expect, it } from "vitest";
import { axisMask, beginOp, describeOp, evaluate, isTyping, pressAxis, typeKey, typedValues, type TransformOp } from "./transform";

const typeAll = (op: TransformOp, keys: string[]): TransformOp => keys.reduce((o, k) => typeKey(o, k) ?? o, op);

describe("axis constraints", () => {
  it("cycles global, local, free on repeated presses", () => {
    let op = beginOp("grab");
    op = pressAxis(op, 0, false);
    expect(op.constraint).toEqual({ axis: 0, plane: false, orientation: "global" });
    op = pressAxis(op, 0, false);
    expect(op.constraint?.orientation).toBe("local");
    op = pressAxis(op, 0, false);
    expect(op.constraint).toBeNull();
  });

  it("switches axis without cycling", () => {
    const op = pressAxis(pressAxis(beginOp("grab"), 0, false), 2, false);
    expect(op.constraint).toEqual({ axis: 2, plane: false, orientation: "global" });
  });

  it("masks the changed axes", () => {
    expect(axisMask(beginOp("resize"))).toEqual([1, 1, 1]);
    expect(axisMask(pressAxis(beginOp("resize"), 1, false))).toEqual([0, 1, 0]);
    expect(axisMask(pressAxis(beginOp("resize"), 2, true))).toEqual([1, 1, 0]);
  });
});

describe("typed values", () => {
  it("types into the constrained axis", () => {
    const op = typeAll(pressAxis(beginOp("grab"), 2, false), ["1", "2", "8"]);
    expect(isTyping(op)).toBe(true);
    expect(typedValues(op)).toEqual([null, null, 128]);
  });

  it("tabs between axes when free", () => {
    const op = typeAll(beginOp("grab"), ["6", "4", "Tab", "3", "2"]);
    expect(typedValues(op)).toEqual([64, 32, null]);
  });

  it("negates the value with minus after a number", () => {
    expect(typedValues(typeAll(beginOp("rotate"), ["9", "0", "-"]))[0]).toBe(-90);
    expect(typedValues(typeAll(beginOp("rotate"), ["9", "0", "-", "-"]))[0]).toBe(90);
    expect(typedValues(typeAll(beginOp("rotate"), ["-", "4", "5"]))[0]).toBe(-45);
  });

  it("backspaces", () => {
    expect(typedValues(typeAll(beginOp("grab"), ["1", "2", "Backspace"]))[0]).toBe(1);
  });

  it("does not claim keys that are not numeric input", () => {
    expect(typeKey(beginOp("grab"), "g")).toBeNull();
  });
});

describe("evaluate", () => {
  it("does arithmetic with precedence and parentheses", () => {
    expect(evaluate("2+3*4")).toBe(14);
    expect(evaluate("(2+3)*4")).toBe(20);
    expect(evaluate("64/2")).toBe(32);
    expect(evaluate("-(1.5)")).toBe(-1.5);
    expect(evaluate(".5")).toBe(0.5);
  });

  it("rejects anything else", () => {
    expect(evaluate("")).toBeNull();
    expect(evaluate("2+")).toBeNull();
    expect(evaluate("alert(1)")).toBeNull();
    expect(evaluate("1/0")).toBeNull();
  });
});

describe("readout", () => {
  it("names the axis and orientation", () => {
    const op = pressAxis(beginOp("grab"), 0, false);
    expect(describeOp(op, [64, 0, 0])).toBe("Dx 64 along global X");
    expect(describeOp(beginOp("rotate"), 90)).toBe("Rotation 90 deg");
  });
});

describe("blender numeric input and axis locking", () => {
  it("takes the reciprocal with slash in simple mode", () => {
    expect(typedValues(typeAll(beginOp("resize"), ["2", "/"]))[0]).toBe(0.5);
    expect(typedValues(typeAll(beginOp("resize"), ["2", "/", "/"]))[0]).toBe(2);
  });

  it("switches to expressions with = and types operators literally", () => {
    const op = typeAll(beginOp("grab"), ["=", "6", "4", "/", "2", "+", "1"]);
    expect(op.advanced).toBe(true);
    expect(typedValues(op)[0]).toBe(33);
  });

  it("steps back an axis with Ctrl+Tab", () => {
    const op = typeAll(beginOp("grab"), ["Tab", "Tab", "ShiftTab", "5"]);
    expect(typedValues(op)).toEqual([null, 5, null]);
  });

  it("locks to the header orientation first, then global", () => {
    let op = pressAxis(beginOp("grab"), 2, false, "local");
    expect(op.constraint?.orientation).toBe("local");
    op = pressAxis(op, 2, false, "local");
    expect(op.constraint?.orientation).toBe("global");
    op = pressAxis(op, 2, false, "local");
    expect(op.constraint).toBeNull();
    op = pressAxis(beginOp("grab"), 0, false, "view");
    expect(op.constraint?.orientation).toBe("view");
    expect(pressAxis(op, 0, false, "view").constraint?.orientation).toBe("global");
  });
});
