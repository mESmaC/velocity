import { commands } from '../bindings';

export type { WorkKey, WorkPiece, WorkPool } from '../bindings';
export const workPoolCommands = { get: commands.getWorkPool };
