import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    projects: [
      { extends: true, test: { name: "node", include: ["test/*.test.ts"] } },
      {
        extends: true,
        test: { name: "browser", include: ["test/view/*.test.ts"], environment: "happy-dom" },
      },
    ],
  },
});
