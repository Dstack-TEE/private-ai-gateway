import { createRoot } from "react-dom/client";
import { Renderer } from "./index";
import "./app.css";

const root = document.getElementById("root");
if (!root) throw new Error("Missing renderer root");
createRoot(root).render(<Renderer />);
