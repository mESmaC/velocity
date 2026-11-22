import {
  commands,
  type DeploySecretStatus_Serialize as DeploySecretStatus,
  type DeployStepSummary_Serialize as DeployStepSummary,
  type ProjectDeploySettings_Serialize as ProjectDeploySettings,
} from '../bindings';

export type { DeploySecretStatus, DeployStepSummary, ProjectDeploySettings };

// Project deploy secrets and `[deploy]` step configuration (#594).
// Secret values are stored only in Windows Credential Manager under
// `velocity/deploy/<project>/<NAME>` and never returned to the window.
export const deployCommands = {
  projectSettings: (project: number) =>
    commands.getProjectDeploySettings(project),
  setSecret: (project: number, name: string, value: string) =>
    commands.setProjectDeploySecret(project, name, value),
  clearSecret: (project: number, name: string) =>
    commands.clearProjectDeploySecret(project, name),
};
