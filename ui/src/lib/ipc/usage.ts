import { commands } from '../bindings';

export type {
  ProcessMemory,
  ProviderUsage,
  UsageWindowView,
} from '../bindings';

export const getProcessMemory = commands.getProcessMemory;
