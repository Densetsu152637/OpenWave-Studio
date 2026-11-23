import { describe, expect, it } from "vitest";
import { beginTransaction, edit, endTransaction, ipcCommand, redo, undo } from "./commands";
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
