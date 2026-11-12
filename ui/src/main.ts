import { mount } from 'svelte';
import Root from './Root.svelte';
import './lib/theme';
import './lib/theme/shell.css';
import { reportWindowError } from './lib/ipc';

const target = document.getElementById('app');
if (!target) throw new Error('Velocity mount target is missing');

// Whatever the panes' boundaries miss still reaches velocity.log (#273).
window.addEventListener('error', (event) =>
  reportWindowError('uncaught', event.error ?? event.message),
);
window.addEventListener('unhandledrejection', (event) =>
  reportWindowError('unhandled rejection', event.reason),
);

mount(Root, { target });
