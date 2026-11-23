import { describe, expect, it } from "vitest";
import {
  analyze,
  beginTransaction,
  edit,
  endTransaction,
  exportMix,
  exportWav,
  ipcCommand,
  mediaImport,
  pluginEditorClose,
  pluginEditorOpen,
  projectNew,
  projectOpen,
  projectSave,
  redo,
  undo,
} from "./commands";
import type { IpcTransport } from "./transport";

/** A transport whose ipc_command calls take the given delays; one can be made to fail. */
function slowTransport(delays: number[], failAt = -1) {
  const arrived: string[] = [];
  const finished: string[] = [];
  let calls = 0;
  const transport: IpcTransport = {
    available: true,
    channel: <T>(handler: (message: T) => void) => ({ onmessage: handler }),
    invoke: (async (_command: string, args?: Record<string, unknown>) => {
      const index = calls++;
      const command = args!.command as { type: string };
      arrived.push(command.type);
      await new Promise((resolve) => setTimeout(resolve, delays[index] ?? 0));
      finished.push(command.type);
      if (index === failAt) throw new Error("refused");
      return { seq: index + 1, ids: [], engineError: null };
    }) as IpcTransport["invoke"],
  };
  return { transport, arrived, finished };
}

describe("command queue", () => {
  it("invokes commands one at a time, in the order they were issued", async () => {
    // The first command is the slowest: without the queue the later ones would overtake it.
    const { transport, arrived, finished } = slowTransport([30, 5, 20, 1, 10]);
    const replies = [
      beginTransaction("Drag", transport),
      edit({ command: "set_name", name: "a" }, transport),
      edit({ command: "set_name", name: "b" }, transport),
      endTransaction(transport),
      undo(transport),
    ];
    const settled = await Promise.all(replies);
    expect(arrived).toEqual(["begin", "edit", "edit", "end", "undo"]);
    expect(finished).toEqual(arrived);
    expect(settled.map((r) => r.seq)).toEqual([1, 2, 3, 4, 5]);
  });

  it("does not jam after a failed command", async () => {
    const { transport, arrived } = slowTransport([5, 1, 1], 0);
    const first = ipcCommand({ type: "undo" }, transport);
    const second = redo(transport);
    const third = ipcCommand({ type: "cancel" }, transport);
    await expect(first).rejects.toThrow("refused");
    await expect(second).resolves.toMatchObject({ seq: 2 });
    await expect(third).resolves.toMatchObject({ seq: 3 });
    expect(arrived).toEqual(["undo", "redo", "cancel"]);
  });
});

describe("project commands", () => {
  it("send the documented command names and arguments, in order with edits", async () => {
    const calls: Array<[string, Record<string, unknown> | undefined]> = [];
    const transport: IpcTransport = {
      available: true,
      channel: <T>(handler: (message: T) => void) => ({ onmessage: handler }),
      invoke: (async (command: string, args?: Record<string, unknown>) => {
        calls.push([command, args]);
        // The edit is the slowest: the import must still wait for it.
        if (command === "ipc_command") await new Promise((resolve) => setTimeout(resolve, 20));
        return { media: 7, frames: 96000, sampleRate: 48000, channels: 2 };
      }) as IpcTransport["invoke"],
    };
    const first = edit({ command: "set_name", name: "a" }, transport);
    const imported = mediaImport("C:/take.wav", transport);
    const saved = projectSave(undefined, transport);
    const saveAs = projectSave("C:/song", transport);
    const opened = projectOpen("C:/song.owave", transport);
    const created = projectNew(transport);
    const exported = exportWav("C:/mix", transport);
    await Promise.all([first, imported, saved, saveAs, opened, created, exported]);
    expect((await imported).media).toBe(7);
    expect(calls.map(([command]) => command)).toEqual([
      "ipc_command",
      "media_import",
      "project_save",
      "project_save",
      "project_open",
      "project_new",
      "export_wav",
    ]);
    expect(calls[1][1]).toEqual({ path: "C:/take.wav" });
    expect(calls[2][1]).toEqual({ path: null });
    expect(calls[3][1]).toEqual({ path: "C:/song" });
    expect(calls[4][1]).toEqual({ path: "C:/song.owave" });
    expect(calls[5][1]).toBeUndefined();
    expect(calls[6][1]).toEqual({ path: "C:/mix" });
  });
});

describe("mix commands", () => {
  it("send the export options and the analysis request through the same queue", async () => {
    const calls: Array<[string, Record<string, unknown> | undefined]> = [];
    const transport: IpcTransport = {
      available: true,
      channel: <T>(handler: (message: T) => void) => ({ onmessage: handler }),
      invoke: (async (command: string, args?: Record<string, unknown>) => {
        calls.push([command, args]);
        if (command === "ipc_command") await new Promise((resolve) => setTimeout(resolve, 20));
        return { path: "C:/mix.wav" };
      }) as IpcTransport["invoke"],
    };
    const first = edit({ command: "set_name", name: "a" }, transport);
    const options = { path: "C:/mix", normalize: { targetLufs: -14, truePeakCeilingDb: -1 } };
    const exported = exportMix(options, transport);
    const plain = exportMix({ path: "C:/plain", normalize: null }, transport);
    const everything = analyze(undefined, transport);
    const strip = analyze({ strip: { bus: 3 }, startBeat: 4, endBeat: 8 }, transport);
    await Promise.all([first, exported, plain, everything, strip]);
    expect(calls.map(([command]) => command)).toEqual([
      "ipc_command",
      "export_mix",
      "export_mix",
      "analyze",
      "analyze",
    ]);
    expect(calls[1][1]).toEqual({ options });
    expect(calls[2][1]).toEqual({ options: { path: "C:/plain", normalize: null } });
    expect(calls[3][1]).toEqual({ request: { strip: null, startBeat: null, endBeat: null } });
    expect(calls[4][1]).toEqual({ request: { strip: { bus: 3 }, startBeat: 4, endBeat: 8 } });
  });
});

describe("plugin editor commands", () => {
  it("invoke the editor commands with the device id", async () => {
    const calls: Array<[string, Record<string, unknown> | undefined]> = [];
    const transport: IpcTransport = {
      available: true,
      channel: <T>(handler: (message: T) => void) => ({ onmessage: handler }),
      invoke: (async (command: string, args?: Record<string, unknown>) => {
        calls.push([command, args]);
      }) as IpcTransport["invoke"],
    };
    await pluginEditorOpen(7, transport);
    await pluginEditorClose(7, transport);
    expect(calls).toEqual([
      ["plugin_editor_open", { device: 7 }],
      ["plugin_editor_close", { device: 7 }],
    ]);
  });
});
