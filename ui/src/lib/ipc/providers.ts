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
  signOut: commands.providerSignOut,
  edit: commands.setProviderSettings,
  menu: commands.getModelMenu,
  choose: commands.chooseModel,
};

export const onProviders = (
  callback: EventCallback<ProvidersChanged_Serialize>,
) => listen(PROVIDERS_EVENT, callback);
