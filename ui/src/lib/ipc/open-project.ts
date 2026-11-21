import { commands } from '../bindings';

export type {
  CollapseRepository_Serialize as CollapseRepository,
  CollapseRepositoryPickerView_Serialize as CollapseRepositoryPickerView,
  GitHubRepository_Serialize as GitHubRepository,
  OpenProjectRequest,
  RepositoryPickerView_Serialize as RepositoryPickerView,
} from '../bindings';

// Open project in the project switcher (SPEC.md F2): a folder you already have,
// one of your GitHub repositories, or an unlinked Collapse repository (#575).
export const openProjectCommands = {
  open: commands.openProject,
  repositories: commands.getGithubRepositories,
  collapseRepositories: commands.getCollapseRepositories,
};
