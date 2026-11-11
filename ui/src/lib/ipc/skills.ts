import { commands } from '../bindings';
export type {
  DiscoveredSkill,
  InstalledSkillPack_Serialize as InstalledSkillPack,
  SkillBundleDefinition,
  SkillDiffCategory,
  SkillLibrarySnapshot_Serialize as SkillLibrarySnapshot,
  SkillPackCatalogEntry_Serialize as SkillPackCatalogEntry,
  SkillPackDiffFile_Serialize as SkillPackDiffFile,
  SkillPackUpdatePreview_Serialize as SkillPackUpdatePreview,
} from '../bindings';

export const skillLibraryCommands = {
  getLibrary: commands.getSkillLibrary,
  installPack: commands.installSkillPack,
  checkUpdate: commands.checkSkillPackUpdate,
  confirmUpdate: commands.confirmSkillPackUpdate,
};

export type {
  SkillsSettingsView_Serialize as SkillsSettingsView,
  SkillSettingsEdit,
} from '../bindings';
export const skillSettingsCommands = {
  read: commands.getSkillSettings,
  edit: commands.setSkillSettings,
};
