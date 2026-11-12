import type { Network, NetworkOption } from "./types.js";

export function toNetwork(option: NetworkOption | undefined): Network {
  if (option === undefined || option === "open") return { mode: "open", hosts: [] };
  if (option === "blocked") return { mode: "blocked", hosts: [] };
  return { mode: "allowlist", hosts: option.allow };
}
