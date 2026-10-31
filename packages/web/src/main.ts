import { mount } from "svelte";
import "./app.css";
import "./lib/theme.svelte"; // initialise theme controller (system listener + persistence)
import "./lib/pwa"; // service worker: pick up a new build on resume, apply it between pages
import App from "./App.svelte";

// Before the app mounts and makes its first request. The browser stops recording once the
// buffer is full (250 entries by default), and the connection page (pages/Debug.svelte) reads
// the launch's requests back out of it, possibly after an hour of use.
performance.setResourceTimingBufferSize?.(2000);

const app = mount(App, { target: document.getElementById("app")! });

export default app;
