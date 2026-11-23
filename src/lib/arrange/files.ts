import type { Snapshot } from "../ipc/types";

/** The native dialogs the file menu uses; each resolves null when the user cancels. */
export interface Dialogs {
  /** Whether to go on although the project has unsaved changes. */
  confirmDiscard(): Promise<boolean>;
  pickProjectToOpen(): Promise<string | null>;
  pickProjectSavePath(defaultName: string): Promise<string | null>;
  pickAudioToImport(): Promise<string | null>;
  pickExportPath(defaultName: string): Promise<string | null>;
}

/** The project commands the file menu issues. */
export interface ProjectOps {
  projectNew(): Promise<Snapshot>;
  projectOpen(path: string): Promise<Snapshot>;
  projectSave(path?: string): Promise<string>;
  exportWav(path: string): Promise<string>;
}

/** The mirror's view of the project the file menu needs. */
export interface FileState {
  dirty: boolean;
  projectPath: string | null;
  name: string;
}

/** The file menu's behavior. Each action resolves true when it did something (false: cancelled). */
export function fileActions(
  ops: ProjectOps,
  dialogs: Dialogs,
  state: () => FileState,
) {
  const discardOk = async () => !state().dirty || (await dialogs.confirmDiscard());

  async function saveAs(): Promise<boolean> {
    const path = await dialogs.pickProjectSavePath(state().name);
    if (!path) return false;
    await ops.projectSave(path);
    return true;
  }

  return {
    async newProject(): Promise<boolean> {
      if (!(await discardOk())) return false;
      await ops.projectNew();
      return true;
    },
    async openProject(): Promise<boolean> {
      if (!(await discardOk())) return false;
      const path = await dialogs.pickProjectToOpen();
      if (!path) return false;
      await ops.projectOpen(path);
      return true;
    },
    /** Saves in place, or asks where when the project has no file yet. */
    async save(): Promise<boolean> {
      if (state().projectPath === null) return saveAs();
      await ops.projectSave();
      return true;
    },
    saveAs,
    async exportWav(): Promise<boolean> {
      const path = await dialogs.pickExportPath(state().name);
      if (!path) return false;
      await ops.exportWav(path);
      return true;
    },
  };
}

export type FileActions = ReturnType<typeof fileActions>;
