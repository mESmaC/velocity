import { commands } from '../bindings';

export type {
  Machine,
  MachinesView_Serialize as MachinesView,
  Role as MachineRole,
} from '../bindings';
export const machineCommands = {
  list: commands.getMachines,
  listen: commands.enableMachineListener,
  createKey: commands.createMachineJoinKey,
  approve: commands.approveMachine,
  revoke: commands.revokeMachine,
  dispatch: commands.dispatchToMachine,
};
