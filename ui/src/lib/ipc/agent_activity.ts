import { commands, type AgentSettings } from '../bindings';
export async function readAgentSettings(): Promise<AgentSettings> {
  return (await commands.getSettings()).agents;
}
export async function saveAgentSettings(
  agents: AgentSettings,
): Promise<AgentSettings> {
  const current = await commands.getSettings();
  return (await commands.setSettings({ ...current, agents })).agents;
}
