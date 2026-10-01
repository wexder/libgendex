import { render } from "solid-js/web";
import { client } from "./api/client.gen";
import App from "./App";
import "./styles.css";

client.setConfig({ baseUrl: "" });

render(() => <App />, document.getElementById("root")!);
