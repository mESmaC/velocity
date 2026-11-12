import { commands } from '../bindings';

export type { ComparisonSide, GraphComparison } from '../bindings';

// The code graph's per-project switch and its token and time comparison (SPEC.md F19),
// shown in the usage modal.
export const codeGraphCommands = {
  comparison: commands.getCodeGraph,
  setEnabled: commands.setCodeGraph,
  reset: commands.resetCodeGraphUsage,
};
