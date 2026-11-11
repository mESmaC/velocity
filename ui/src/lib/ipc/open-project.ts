import { commands } from '../bindings';

export type {
  GitHubRepository_Serialize as GitHubRepository,
  OpenProjectRequest,
  RepositoryPickerView_Serialize as RepositoryPickerView,
} from '../bindings';

// Open project in the project switcher (SPEC.md F2): a folder you already have, or
// one of your GitHub repositories. Rust searches the repository list.
export const openProjectCommands = {
  open: commands.openProject,
  repositories: commands.getGithubRepositories,
};
