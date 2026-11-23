import { describe, expect, it } from "vitest";
import type { Snapshot } from "../ipc/types";
import { fileActions, type Dialogs, type FileState, type ProjectOps } from "./files";

function setup(state: FileState, answers: Partial<Record<keyof Dialogs, unknown>> = {}) {
  const calls: string[] = [];
  const snapshot = {} as Snapshot;
  const ops: ProjectOps = {
    projectNew: async () => (calls.push("new"), snapshot),
    projectOpen: async (path) => (calls.push(`open:${path}`), snapshot),
    projectSave: async (path) => (calls.push(`save:${path ?? ""}`), path ?? "saved"),
    exportWav: async (path) => (calls.push(`export:${path}`), path),
  };
  const answer = (name: keyof Dialogs, fallback: unknown) =>
    name in answers ? answers[name] : fallback;
  const dialogs: Dialogs = {
    confirmDiscard: async () => (calls.push("confirm"), answer("confirmDiscard", true) as boolean),
    pickProjectToOpen: async () => answer("pickProjectToOpen", "/p/a.owave") as string | null,
    pickProjectSavePath: async (name) => (calls.push(`savepath:${name}`), answer("pickProjectSavePath", "/p/b.owave") as string | null),
    pickAudioToImport: async () => null,
    pickExportPath: async (name) => (calls.push(`exportpath:${name}`), answer("pickExportPath", "/p/out.wav") as string | null),
  };
  return { actions: fileActions(ops, dialogs, () => state), calls };
}

const clean: FileState = { dirty: false, projectPath: "/p/a.owave", name: "Song" };
const dirty: FileState = { ...clean, dirty: true };

describe("file menu", () => {
  it("starts a new project without asking when nothing is unsaved", async () => {
    const { actions, calls } = setup(clean);
    expect(await actions.newProject()).toBe(true);
    expect(calls).toEqual(["new"]);
  });

  it("asks before New and Open when there are unsaved changes", async () => {
    const a = setup(dirty);
    expect(await a.actions.newProject()).toBe(true);
    expect(a.calls).toEqual(["confirm", "new"]);
    const b = setup(dirty);
    expect(await b.actions.openProject()).toBe(true);
    expect(b.calls).toEqual(["confirm", "open:/p/a.owave"]);
  });

  it("keeps the project when the discard prompt is declined", async () => {
    const a = setup(dirty, { confirmDiscard: false });
    expect(await a.actions.newProject()).toBe(false);
    expect(await a.actions.openProject()).toBe(false);
    expect(a.calls).toEqual(["confirm", "confirm"]);
  });

  it("opens nothing when the open dialog is cancelled", async () => {
    const { actions, calls } = setup(clean, { pickProjectToOpen: null });
    expect(await actions.openProject()).toBe(false);
    expect(calls).toEqual([]);
  });

  it("saves in place, or asks where for a project without a file", async () => {
    const a = setup(clean);
    expect(await a.actions.save()).toBe(true);
    expect(a.calls).toEqual(["save:"]);
    const b = setup({ ...clean, projectPath: null });
    expect(await b.actions.save()).toBe(true);
    expect(b.calls).toEqual(["savepath:Song", "save:/p/b.owave"]);
  });

  it("saves to a chosen place with Save As, and not when cancelled", async () => {
    const a = setup(clean);
    expect(await a.actions.saveAs()).toBe(true);
    expect(a.calls).toEqual(["savepath:Song", "save:/p/b.owave"]);
    const b = setup(clean, { pickProjectSavePath: null });
    expect(await b.actions.save()).toBe(true);
    expect(await b.actions.saveAs()).toBe(false);
    expect(b.calls).toEqual(["save:", "savepath:Song"]);
  });

  it("exports to the chosen file", async () => {
    const a = setup(dirty);
    expect(await a.actions.exportWav()).toBe(true);
    expect(a.calls).toEqual(["exportpath:Song", "export:/p/out.wav"]);
    const b = setup(dirty, { pickExportPath: null });
    expect(await b.actions.exportWav()).toBe(false);
  });
});
