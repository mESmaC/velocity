import { commands } from '../bindings';

export type {
  AddGoalAgentsRequest,
  BoardPieceKind,
  BoardRowState,
  GoalAgentCard_Serialize as GoalAgentCard,
  GoalBoardRow_Serialize as GoalBoardRow,
  GoalClaimsBoard_Serialize as GoalClaimsBoard,
  GoalEventKind,
  GoalFeedEvent,
  GoalSwarmViewSnapshot_Serialize as GoalSwarmViewSnapshot,
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

// Goal swarm dialog, launch, + Agents and the view's claims board (SPEC.md F16).
export const goalSwarmCommands = {
  dialog: commands.getGoalSwarmDialog,
  readPool: commands.readGoalSwarmPool,
  launch: commands.launchGoalSwarm,
  addAgents: commands.addGoalAgents,
  board: (swarm: string) => commands.getGoalSwarmBoard(Number(swarm)),
};
