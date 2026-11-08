import javascript from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import typescript from 'typescript-eslint';
import svelteConfig from './svelte.config.js';

export default typescript.config(
  { ignores: ['dist/**', 'node_modules/**', 'src/lib/bindings.ts'] },
  javascript.configs.recommended,
  ...typescript.configs.recommended,
  ...svelte.configs['flat/recommended'],
  {
    files: ['**/*.svelte'],
    languageOptions: {
      parserOptions: { parser: typescript.parser, svelteConfig },
    },
  },
);
