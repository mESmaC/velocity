import { commands } from '../bindings';

export type {
  DispatchView_Serialize as DispatchView,
  DispatchIssue,
} from '../bindings';
export const continuousDispatchCommands = {
  get: commands.getContinuousDispatch,
  set: commands.setContinuousDispatch,
};
