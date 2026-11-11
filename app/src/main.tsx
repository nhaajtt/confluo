import { render } from "solid-js/web";
import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import "./styles.css";
import App from "./App";
import { init } from "./store";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");
render(() => <App />, root);
void init().catch((e) => console.error("startup failed", e));
