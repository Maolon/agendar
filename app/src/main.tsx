import { createRoot } from "react-dom/client";
import { App } from "./App";
import { resolveClient, surfaceFromLocation } from "./api/client";
import { SnapshotStore } from "./api/store";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/layout.css";
import "./styles/cards.css";

const container = document.getElementById("root");
if (!container) throw new Error("Missing #root element");

const root = createRoot(container);

void resolveClient().then((client) => {
  const store = new SnapshotStore(client);
  root.render(
    <App client={client} store={store} surface={surfaceFromLocation(window.location.search)} />,
  );
});
