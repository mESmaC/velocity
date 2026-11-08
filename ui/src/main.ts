import { mount } from 'svelte';
import App from './App.svelte';
import './lib/theme';
import './lib/theme/shell.css';

const target = document.getElementById('app');
if (!target) throw new Error('Velocity mount target is missing');

mount(App, { target });
