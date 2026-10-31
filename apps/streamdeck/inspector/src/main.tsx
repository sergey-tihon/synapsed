import { render } from "@solidjs/web";
import App from "./App.js";

const root = document.getElementById("root");
if (!root) throw new Error("Inspector root element is missing.");
render(() => <App />, root);
