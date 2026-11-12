import { Kumoba } from "kumoba";

// Needs KUMOBA_URL and KUMOBA_TOKEN (or KUMOBA_TOKEN_FILE); scripts/serve.sh writes them to .local/serve/env.
const kumoba = Kumoba.fromEnv();
console.log("images:", await kumoba.images());

await using sandbox = await kumoba.create({
  image: process.env.KUMOBA_IMAGE ?? "default",
  cpu: 2,
  memoryMib: 2048,
  user: "kumoba",
  network: "blocked",
  labels: { example: "quickstart" },
});
console.log("sandbox", sandbox.id, sandbox.state);

const uname = await sandbox.run("uname -a");
console.log(uname.stdout.trim());

await sandbox.files.write("/workspace/hello.py", 'print("hello from a microVM")\n');
const python = await sandbox.run(["python3", "/workspace/hello.py"], {
  onStdout: (text) => process.stdout.write(text),
});
if (!python.ok) console.error("python failed:", python.stderr);

console.log(await sandbox.files.list("/workspace"));

const slow = await sandbox.run("sleep 30", { timeoutMs: 1000 });
console.log("timed out:", slow.timedOut);
