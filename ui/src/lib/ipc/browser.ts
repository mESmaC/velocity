import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  BROWSER_FRAME_EVENT,
  commands,
  type BrowserFrame_Serialize as BrowserFrame,
  type BrowserInput,
  type BrowserView_Serialize as BrowserView,
} from '../bindings';

export type { BrowserFrame, BrowserInput, BrowserView };

export const browserCommands = {
  open: commands.openBrowserView,
  close: commands.closeBrowserView,
  takeOver: commands.takeOverBrowser,
  handBack: commands.handBackBrowser,
  input: commands.browserInput,
  switchTab: commands.switchBrowserTab,
};

export function onBrowserFrame(handler: EventCallback<BrowserFrame>) {
  return listen<BrowserFrame>(BROWSER_FRAME_EVENT, handler);
}
