import { commands, type ProofSettings } from '../bindings';

export type {
  DesignProofEntry,
  LanguageTranslator_Serialize as LanguageTranslator,
  ProofListFeature,
  ProofsSettingsView_Serialize as ProofsSettingsView,
} from '../bindings';

// Settings → Proofs (SPEC.md F13): the page reads the project's checkout, and the
// stress-diff input count is saved with the rest of the settings.
export const proofsSettingsCommands = {
  read: commands.getProofsSettings,
};

export async function saveProofSettings(
  proofs: ProofSettings,
): Promise<ProofSettings> {
  const current = await commands.getSettings();
  return (await commands.setSettings({ ...current, proofs })).proofs;
}
