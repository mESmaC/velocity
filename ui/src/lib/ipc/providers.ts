import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  PROVIDERS_EVENT,
  type ProvidersChanged_Serialize,
} from '../bindings';

export type {
  LimitView,
  ModelCardView,
  ModelChipView,
  ModelChoiceView_Serialize as ModelChoiceView,
  ModelMenuChoice,
  ModelMenuGroup,
  ModelMenuItem,
  ModelMenuView_Serialize as ModelMenuView,
  ProviderCardView_Serialize as ProviderCardView,
  ProviderEdit,
  ProviderPhase,
  ProvidersChanged_Serialize as ProvidersChanged,
  ProvidersView_Serialize as ProvidersView,
  SignInCardView,
} from '../bindings';

// Settings → Providers and the composer's model menu (SPEC.md F15, F4).
export const providerCommands = {
  list: commands.listProviders,
  signIn: commands.providerSignIn,
  // Sign out, and Cancel while signing in or installing.
  signOut: commands.providerSignOut,
  // F15 Install: puts a built-in provider's missing agent on PATH.
  install: commands.installProvider,
  edit: commands.setProviderSettings,
  menu: commands.getModelMenu,
  choose: commands.chooseModel,
  // F15: edit an agent's command (null resets a built-in one), Add an ACP agent, remove one.
  setCommand: commands.setProviderCommand,
  addAgent: commands.addCustomAgent,
  removeAgent: commands.removeCustomAgent,
};

/** The mark a provider's icon and colour use: a built-in provider's own, or the generic
 *  agent mark for an ACP agent you added (F15 Custom ACP agents). */
export function providerMark(provider: string): string {
  return provider.startsWith('custom-') ? 'custom' : provider;
}

export function providerIcon(provider: string): string {
  return provider.startsWith('custom-') ? 'agents' : provider;
}

export const onProviders = (
  callback: EventCallback<ProvidersChanged_Serialize>,
) => listen(PROVIDERS_EVENT, callback);
