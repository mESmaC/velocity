import { commands } from '../bindings';

export type {
  RosenChangeStage,
  RosenChangeView,
  RosenThreadAction,
} from '../bindings';

export const threadChangeCommands = {
  run: commands.runRosenThreadAction,
};
