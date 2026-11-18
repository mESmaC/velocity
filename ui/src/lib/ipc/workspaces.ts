import { commands } from '../bindings';

export type {
  LandedWorkspacePreparation,
  WorkspacePreparation,
  WorkspaceRow,
  WorktreePathPreview,
  WorktreeSettingsEdit,
  WorktreeSettingsView_Serialize as WorkspacePreferences,
} from '../bindings';

export const workspaceCommands = {
  settings: commands.getWorkspaceSettings,
  set: commands.setWorkspaceSettings,
  preview: commands.previewWorkspaceFolder,
  browse: commands.browseWorkspaceFolder,
  list: commands.listWorkspaces,
  delete: commands.deleteWorkspace,
  preparation: commands.getWorkspacePreparation,
  propose: commands.proposeWorkspacePreparation,
};
