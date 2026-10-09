import { render } from "solid-js/web";
import "./ui/styles.css";
import { App } from "./ui/App";
import { enableControl } from "./control/link";
import { Editor } from "./state/editor";
import { GpuError, unsupportedMessage } from "./render/device";
import { setState } from "./state/stores";
import { views } from "./ui/viewRegistry";

const root = document.getElementById("root");
if (!root) throw new Error("the page has no #root element");
const mount = root;

function fatal(message: string): void {
  document.body.innerHTML =
    '<div class="view-overlay fatal" role="alert"><h2>r5ed cannot start</h2><p></p></div>';
  const p = document.querySelector(".view-overlay p");
  if (p) p.textContent = message;
}

async function main(): Promise<void> {
  const editor = new Editor();
  (globalThis as { r5ed?: Editor }).r5ed = editor;
  (globalThis as { r5edViews?: typeof views }).r5edViews = views;
  try {
    await editor.init();
  } catch (e) {
    fatal(
      e instanceof GpuError
        ? unsupportedMessage(e.message)
        : `the wasm core did not load: ${e instanceof Error ? e.message : String(e)}`,
    );
    return;
  }
  setState("ui", "theme", window.matchMedia?.("(prefers-color-scheme: light)").matches ? "light" : "dark");
  enableControl(editor);
  render(() => App({ editor }), mount);
}

void main();
