import { commands } from '../bindings';
export type {
  DesignSystemStatus_Serialize as DesignSystemStatus,
  GameEngineDetection_Serialize as GameEngineDetection,
  SkillBundleCount_Serialize as SkillBundleCount,
  SkillPackBundleCounts_Serialize as SkillPackBundleCounts,
} from '../bindings';

// Built-in pack specifics (SPEC.md F18): UI UX Pro Max's design system and the Game dev
// skills engine detection. Bundle counts arrive on the skill library snapshot.
export const skillPackCommands = {
  getDesignSystem: commands.getDesignSystem,
  generateDesignSystem: commands.generateDesignSystem,
  detectGameEngine: commands.detectGameEngine,
};
