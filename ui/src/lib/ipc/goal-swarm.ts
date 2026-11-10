import { commands } from '../bindings';

export type {
  AddGoalAgentsRequest,
  GoalDialogRequest,
  GoalLimitLine,
  GoalMixEdit,
  GoalMixRowView,
  GoalMixSegment,
  GoalModelOption,
  GoalSwarmChoices,
  GoalSwarmDialog_Serialize as GoalSwarmDialog,
  LaunchGoalSwarmRequest,
} from '../bindings';

// Goal swarm dialog and launch commands (SPEC.md F16).
export const goalSwarmCommands = {
  dialog: commands.getGoalSwarmDialog,
  readPool: commands.readGoalSwarmPool,
  launch: commands.launchGoalSwarm,
  addAgents: commands.addGoalAgents,
};
