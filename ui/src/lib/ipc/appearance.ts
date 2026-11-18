import { commands } from '../bindings';

export type { UiZoomAdjustment, UiZoomView } from '../bindings';

export const appearanceCommands = {
  readZoom: commands.getUiZoom,
  adjustZoom: commands.adjustUiZoom,
};
