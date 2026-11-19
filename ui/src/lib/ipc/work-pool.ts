import { commands } from '../bindings';

export type {
  WorkChange,
  WorkItem,
  WorkKey,
  WorkPiece,
  WorkPool,
} from '../bindings';
export const workPoolCommands = {
  get: commands.getWorkPool,
  change: commands.getWorkChange,
};
