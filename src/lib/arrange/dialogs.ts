import { ask, open, save } from "@tauri-apps/plugin-dialog";
import type { Dialogs } from "./files";

/** File types the importer decodes. */
export const AUDIO_EXTENSIONS = ["wav", "aif", "aiff", "flac", "mp3", "ogg"];

/** A project is a directory bundle, so opening one picks a folder. */
const nativeDialogs: Dialogs = {
  confirmDiscard: () =>
    ask("The project has unsaved changes. Discard them?", {
      title: "Unsaved changes",
      kind: "warning",
      okLabel: "Discard",
      cancelLabel: "Cancel",
    }),
  pickProjectToOpen: async () => {
    const picked = await open({ directory: true, multiple: false, title: "Open project (.owave folder)" });
    return typeof picked === "string" ? picked : null;
  },
  pickProjectSavePath: (defaultName) =>
    save({
      title: "Save project",
      defaultPath: `${defaultName}.owave`,
      filters: [{ name: "OpenWave project", extensions: ["owave"] }],
    }),
  pickAudioToImport: async () => {
    const picked = await open({
      multiple: false,
      title: "Import audio",
      filters: [{ name: "Audio", extensions: AUDIO_EXTENSIONS }],
    });
    return typeof picked === "string" ? picked : null;
  },
  pickExportPath: (defaultName) =>
    save({
      title: "Export WAV",
      defaultPath: `${defaultName}.wav`,
      filters: [{ name: "WAV", extensions: ["wav"] }],
    }),
};

export { nativeDialogs };
