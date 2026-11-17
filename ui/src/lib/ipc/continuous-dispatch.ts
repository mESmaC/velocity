import { commands } from '../bindings';

export type {
  DispatchView_Serialize as DispatchView,
  DispatchPreview,
} from '../bindings';
export const continuousDispatchCommands = {
  get: commands.getContinuousDispatch,
  set: commands.setContinuousDispatch,
};
