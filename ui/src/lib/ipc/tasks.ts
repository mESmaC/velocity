import { commands } from '../bindings';
export type { TaskView } from '../bindings';
export const taskCommands = { add: commands.addTask, list: commands.listTasks };
