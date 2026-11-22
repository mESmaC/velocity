import { listen, type EventCallback } from '@tauri-apps/api/event';
import { commands, SWARM_EVENT, type SwarmEventPublished } from '../bindings';

export type {
  LaunchSwarmRequest_Deserialize as LaunchSwarmRequest,
  SwarmCard_Serialize as SwarmCard,
  SwarmDialog_Serialize as SwarmDialog,
  SwarmDialogItem,
  SwarmDialogQuery_Deserialize as SwarmDialogQuery,
  SwarmDialogRequest_Serialize as SwarmDialogRequest,
  SwarmEventKind,
  SwarmEventPublished,
  SwarmFeedEvent,
  SwarmItem,
  SwarmLegendItem,
  SwarmModelCard,
  SwarmSource,
  SwarmStage,
  SwarmViewSnapshot_Serialize as SwarmViewSnapshot,
  TrackStep,
} from '../bindings';

// Swarm dialog, launch, + Items, and the swarm activity feed (SPEC.md F10).
export const swarmCommands = {
  dialog: commands.getSwarmDialog,
  launch: commands.launchSwarm,
  addItems: commands.addToSwarm,
};

export const onSwarmEvent = (callback: EventCallback<SwarmEventPublished>) =>
  listen(SWARM_EVENT, callback);
