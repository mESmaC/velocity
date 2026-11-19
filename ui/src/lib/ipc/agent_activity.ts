import {
  commands,
  type EffortLevel,
  type Settings_Serialize,
} from '../bindings';

export type AgentPreferences = {
  agents: Settings_Serialize['agents'];
  implementationEffort: EffortLevel;
  titleEffort: EffortLevel;
};

export async function readAgentSettings(): Promise<AgentPreferences> {
  const { agents, providers } = await commands.getSettings();
  return {
    agents,
    implementationEffort: providers.default_effort,
    titleEffort: providers.title_effort,
  };
}

export async function saveAgentSettings({
  agents,
  implementationEffort,
  titleEffort,
}: AgentPreferences): Promise<AgentPreferences> {
  const current = await commands.getSettings();
  // The shared MCP servers and the Collapse switch are edited on their own; keep
  // the saved values, not a stale copy.
  const shared_mcp_servers = current.agents.shared_mcp_servers ?? [];
  const collapse_mcp = current.agents.collapse_mcp;
  const saved = await commands.setSettings({
    ...current,
    agents: { ...agents, shared_mcp_servers, collapse_mcp },
    providers: {
      ...current.providers,
      default_effort: implementationEffort,
      title_effort: titleEffort,
    },
  });
  return {
    agents: saved.agents,
    implementationEffort: saved.providers.default_effort,
    titleEffort: saved.providers.title_effort,
  };
}
