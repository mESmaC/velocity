import { commands, type AgentSettings } from '../bindings';
export async function readAgentSettings(): Promise<AgentSettings> {
  return (await commands.getSettings()).agents;
}
export async function saveAgentSettings(
  agents: AgentSettings,
): Promise<AgentSettings> {
  const current = await commands.getSettings();
  // The shared MCP servers are edited on their own; keep the saved list, not a stale copy.
  const shared_mcp_servers = current.agents.shared_mcp_servers ?? [];
  return (
    await commands.setSettings({
      ...current,
      agents: { ...agents, shared_mcp_servers },
    })
  ).agents;
}
