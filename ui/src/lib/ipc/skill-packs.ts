import { commands } from '../bindings';
export type {
  DesignSystemStatus_Serialize as DesignSystemStatus,
  GameEngineDetection_Serialize as GameEngineDetection,
  SkillMcpServer_Serialize as SkillMcpServer,
  SkillPackDetails_Serialize as SkillPackDetails,
} from '../bindings';

// Built-in pack specifics (SPEC.md F18). The design system row, the engine line and REA's
// shared server arrive on each Settings → Skills card as `details`.
export const skillPackCommands = {
  generateDesignSystem: commands.generateDesignSystem,
};
