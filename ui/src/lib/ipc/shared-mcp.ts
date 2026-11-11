import {
  commands,
  type SharedMcpServer,
  type SharedMcpServerEdit,
} from '../bindings';
export type {
  SharedMcpServer,
  SharedMcpServerEdit,
  SharedMcpTransport,
} from '../bindings';
/** Settings → Agents → shared MCP servers (SPEC.md §3.2 Tool access). */
export async function readSharedMcpServers(): Promise<SharedMcpServer[]> {
  return (await commands.getSettings()).agents.shared_mcp_servers ?? [];
}
export async function editSharedMcpServers(
  edit: SharedMcpServerEdit,
): Promise<SharedMcpServer[]> {
  return commands.editSharedMcpServers(edit);
}
