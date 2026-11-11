import { commands } from '../bindings';
export type {
  OutlineAction_Deserialize as OutlineAction,
  OutlineSnapshot_Serialize as OutlineSnapshot,
  OutlineLine_Serialize as OutlineLine,
  InlineText_Serialize as InlineText,
  OutlineFocus,
  RewriteView_Serialize as RewriteView,
  FlaggedRule,
  CoverageItem,
  LintKind,
} from '../bindings';

export const agentsCommands = {
  read: commands.readAgentsMd,
  reload: commands.reloadAgentsMd,
  edit: commands.editAgentsMd,
  save: commands.writeAgentsMd,
  tighten: commands.tightenRule,
};
