import { commands } from '../bindings';

// Cut release (#431). Rust reads Rosen's last green commit, waits until GitHub has it
// and starts the release workflow, which creates the tag on the commit itself. A null
// commit releases the last green commit; any other must be a land it covers.
export const releaseCommands = {
  cut: (project: number, version: string, commit: string | null = null) =>
    commands.cutRelease(project, version, commit),
};
