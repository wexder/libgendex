import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  server: {
    proxy: { "/api": process.env.BOOKJEV_API ?? "http://localhost:8080" },
  },
  build: { target: "es2022" },
});
