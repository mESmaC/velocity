import { commands } from '../bindings';

export type {
  FleetImprovementOutcomeView,
  FleetOffenderView,
  FleetStatusView,
  FleetWindowView,
  ImprovementVerdict,
} from '../bindings';

export const getFleetStats = commands.getFleetStats;
