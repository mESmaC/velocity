import { commands } from '../bindings';

export type {
  ThreadMenuAction,
  ThreadMenuEntry_Serialize as ThreadMenuEntry,
  ThreadMenuView_Serialize as ThreadMenuView,
  ToastView_Serialize as ToastView,
} from '../bindings';

// The sidebar thread menu and Delete with Undo (SPEC.md F12).
export const threadMenuCommands = {
  get: commands.getThreadMenu,
  run: commands.runThreadMenuAction,
  delete: commands.deleteThread,
  undoDelete: commands.undoDeleteThread,
};
