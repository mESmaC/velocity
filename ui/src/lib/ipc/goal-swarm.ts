import { commands } from '../bindings';

export type {
  GoalDialogRequest,
  GoalLimitLine,
  GoalMixEdit,
  GoalMixRowView,
  GoalMixSegment,
  GoalModelOption,
  GoalSwarmChoices,
  GoalSwarmDialog_Serialize as GoalSwarmDialog,
} from '../bindings';

// Goal swarm dialog: drawn in Rust from the user's choices (SPEC.md F16).
export const goalSwarmCommands = {
  dialog: commands.getGoalSwarmDialog,
  readPool: commands.readGoalSwarmPool,
};
