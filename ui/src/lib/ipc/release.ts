import {
  commands,
  type CollapseReleaseTokenSource,
  type ReleaseCredentialsStatus_Serialize as ReleaseCredentialsStatus,
} from '../bindings';

export type { CollapseReleaseTokenSource, ReleaseCredentialsStatus };

// Cut release (#431, #593, #607). For projects mirrored to Collapse, Rust builds and signs
// locally with Windows Credential Manager's updater key and publishes to Collapse
// (including rolling updater/latest.json). GitHub-only projects dispatch release.yml.
export const releaseCommands = {
  cut: (project: number, version: string, commit: string | null = null) =>
    commands.cutRelease(project, version, commit),
  credentialsStatus: () => commands.getReleaseCredentialsStatus(),
  setUpdaterSigningKey: (privateKey: string, password: string | null = null) =>
    commands.setUpdaterSigningKey(privateKey, password),
  clearUpdaterSigningKey: () => commands.clearUpdaterSigningKey(),
  setCollapseReleaseToken: (token: string) =>
    commands.setCollapseReleaseToken(token),
  replaceCollapseReleaseToken: () => commands.replaceCollapseReleaseToken(),
  clearCollapseReleaseToken: () => commands.clearCollapseReleaseToken(),
};
