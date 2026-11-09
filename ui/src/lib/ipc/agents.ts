import { commands } from '../bindings';
export type {
  OutlineAction_Deserialize as OutlineAction,
  OutlineSnapshot_Serialize as OutlineSnapshot,
  OutlineLine,
  OutlineFocus,
  RewriteView,
} from '../bindings';

export const agentsCommands = {
  read: commands.readAgentsMd,
  edit: commands.editAgentsMd,
  save: commands.writeAgentsMd,
  tighten: commands.tightenRule,
};
