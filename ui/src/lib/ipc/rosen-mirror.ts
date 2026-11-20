import { commands, type LayoutSnapshot_Serialize } from '../bindings';
export type { MirrorState_Serialize as MirrorState } from '../bindings';

// The project's Rosen base trails its mirror while a catch-up is pending (#520).
export type MirrorBehind = NonNullable<
  LayoutSnapshot_Serialize['rosen_mirror_behind']
>;

// The base and mirror branch the project's last catch-up found diverged (#501).
export type MirrorDivergence = NonNullable<
  LayoutSnapshot_Serialize['rosen_mirror_divergence']
>;

// Catch up from mirror (#501) and adoption of a ported mirror tip (Rosen #396).
// Rust takes the tip from the project's saved divergence; the window passes only
// the project, and Rosen's answer comes back as a toast or a refusal.
export const rosenMirrorCommands = {
  catchUp: (project: number): Promise<null> =>
    commands.catchUpFromMirror(project),
  adopt: (project: number): Promise<null> => commands.adoptMirrorTip(project),
};
