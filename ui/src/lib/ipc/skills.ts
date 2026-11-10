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
