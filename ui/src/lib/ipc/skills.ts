import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  SKILL_PACK_INSTALLATIONS_EVENT,
  type SkillPackInstallationsChanged,
} from '../bindings';
export type {
  DiscoveredSkill,
  InstalledSkillPack_Serialize as InstalledSkillPack,
  SkillBundleDefinition,
  SkillDiffCategory,
  SkillLibrarySnapshot_Serialize as SkillLibrarySnapshot,
  SkillPackCatalogEntry_Serialize as SkillPackCatalogEntry,
  SkillPackDiffFile_Serialize as SkillPackDiffFile,
  SkillPackInstallationsChanged,
  SkillPackUpdatePreview_Serialize as SkillPackUpdatePreview,
} from '../bindings';

export const skillLibraryCommands = {
  getLibrary: commands.getSkillLibrary,
  installPack: commands.installSkillPack,
  removePack: commands.removeSkillPack,
  setBundleDefault: commands.setSkillBundleDefault,
  installations: commands.getSkillPackInstallations,
  checkUpdate: commands.checkSkillPackUpdate,
  confirmUpdate: commands.confirmSkillPackUpdate,
};

export const onSkillPackInstallations = (
  callback: EventCallback<SkillPackInstallationsChanged>,
) => listen(SKILL_PACK_INSTALLATIONS_EVENT, callback);

export type {
  SkillsSettingsView_Serialize as SkillsSettingsView,
  SkillSettingsEdit,
} from '../bindings';
export const skillSettingsCommands = {
  read: commands.getSkillSettings,
  edit: commands.setSkillSettings,
};
