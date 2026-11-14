import { mount } from "svelte";
import "./app.css";
import "./lib/theme.svelte"; // initialise theme controller (system listener + persistence)
import "./lib/pwa"; // service worker: pick up a new build on resume, apply it between pages
import App from "./App.svelte";

const app = mount(App, { target: document.getElementById("app")! });

export default app;
