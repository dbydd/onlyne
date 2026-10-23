import { mount } from 'svelte';
import '@xyflow/svelte/dist/style.css';
import './styles/tokens.css';
import './styles/base.css';
import App from './App.svelte';

// The base layer first, then xyflow's stylesheet, so the canvas' rules land
// where they can be themed with the same tokens as everything else. The graph
// carries its own overrides in `graph/graph.css`.
export default mount(App, { target: document.getElementById('app')! });
