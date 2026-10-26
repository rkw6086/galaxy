#!/usr/bin/env tsx
import React, { useState, useEffect, useCallback } from "react";
import { render, Text, Box } from "ink";
import Spinner from "ink-spinner";
import { spawn, execFileSync, execSync } from "child_process";
import path from "path";
import { prepareGalaxyBuild } from "../../lib/prepare-galaxy-build.mjs";
import fs from "fs";

// ─── Configuration ───────────────────────────────────────────────────────────

// Workspace root is three levels up from script/build-and-install-to-applications/src
const WORKSPACE_ROOT = path.resolve(import.meta.dirname, "../../..");

// ─── Types ───────────────────────────────────────────────────────────────────

type StepStatus = "pending" | "running" | "done" | "error";

interface Step {
  label: string;
  status: StepStatus;
  detail?: string;
  logs?: string[];
}

const MAX_LOG_LINES = 15;

function runCommandStreaming(
  cmd: string,
  cwd: string,
  onLog: (line: string) => void,
  interactive = false
): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, {
      cwd,
      shell: true,
      stdio: interactive ? "inherit" : ["ignore", "pipe", "pipe"],
    });

    const createChunkProcessor = () => {
      let leftover = "";
      return {
        process(chunk: Buffer) {
          const text = leftover + chunk.toString();
          // Cargo renders progress with carriage returns when attached to a terminal. Treat both
          // carriage returns and newlines as line boundaries so build output appears live in Ink.
          const lines = text.split(/\r\n|\r|\n/);
          leftover = lines.pop() || "";
          for (const line of lines) {
            if (line.trim()) onLog(line);
          }
        },
        flush() {
          if (leftover.trim()) onLog(leftover);
          leftover = "";
        },
      };
    };
    const stdout = createChunkProcessor();
    const stderr = createChunkProcessor();

    child.stdout?.on("data", (chunk: Buffer) => stdout.process(chunk));
    child.stderr?.on("data", (chunk: Buffer) => stderr.process(chunk));

    child.on("close", (code) => {
      stdout.flush();
      stderr.flush();
      if (code === 0) resolve();
      else reject(new Error(`Command failed with exit code ${code}`));
    });

    child.on("error", reject);
  });
}

function isAppRunning(processName: string): boolean {
  try {
    execFileSync("pgrep", ["-x", processName], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

async function waitForAppToExit(processName: string, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!isAppRunning(processName)) return true;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return !isAppRunning(processName);
}

async function stopApp(processName: string, onLog: (line: string) => void): Promise<void> {
  if (!isAppRunning(processName)) return;

  onLog(`${processName} is running. Stopping it before replacing the app...`);
  try {
    execFileSync("pkill", ["-TERM", "-x", processName], { stdio: "ignore" });
  } catch {
    // The process may exit between the running check and the signal.
  }

  if (await waitForAppToExit(processName, 2000)) return;

  onLog(`${processName} did not exit after SIGTERM. Force killing it...`);
  try {
    execFileSync("pkill", ["-KILL", "-x", processName], { stdio: "ignore" });
  } catch {
    // Verify the process state below instead of relying on pkill's exit status.
  }

  if (!(await waitForAppToExit(processName, 1000))) {
    throw new Error(`Unable to stop ${processName} before updating /Applications`);
  }
}

// ─── UI Component ────────────────────────────────────────────────────────────

function StepLine({ step }: { step: Step }) {
  const icon =
    step.status === "pending"
      ? "○"
      : step.status === "running"
        ? ""
        : step.status === "done"
          ? "✓"
          : "✗";

  const color =
    step.status === "pending"
      ? "gray"
      : step.status === "running"
        ? "cyan"
        : step.status === "done"
          ? "green"
          : "red";

  return (
    <Box flexDirection="column">
      <Box>
        {step.status === "running" ? (
          <Text color="cyan">
            <Spinner type="dots" />{" "}
          </Text>
        ) : (
          <Text color={color}>{icon} </Text>
        )}
        <Text color={color}>{step.label}</Text>
        {step.detail && <Text color="gray"> — {step.detail}</Text>}
      </Box>
      {step.logs && step.logs.length > 0 && (
        <Box flexDirection="column" marginLeft={3}>
          {step.logs.map((line, i) => (
            <Text key={i} color="gray" dimColor>
              {line}
            </Text>
          ))}
        </Box>
      )}
    </Box>
  );
}

function App() {
  const [steps, setSteps] = useState<Step[]>([
    { label: "Check working directory", status: "pending" },
    { label: "Pull current branch", status: "pending" },
    { label: "Bundle Galaxy", status: "pending" },
    { label: "Copy to /Applications", status: "pending" },
    { label: "Launch Galaxy", status: "pending" },
  ]);

  const updateStep = useCallback((index: number, update: Partial<Step>) => {
    setSteps((prev) => prev.map((s, i) => (i === index ? { ...s, ...update } : s)));
  }, []);

  const appendLog = useCallback((index: number, line: string) => {
    setSteps((prev) =>
      prev.map((s, i) => {
        if (i !== index) return s;
        const logs = [...(s.logs || []), line].slice(-MAX_LOG_LINES);
        return { ...s, logs };
      })
    );
  }, []);

  useEffect(() => {
    (async () => {
      let prepared: Awaited<ReturnType<typeof prepareGalaxyBuild>> | undefined;
      try {
        const appName = "Galaxy.app";
        const destPath = path.join("/Applications", appName);

        prepared = await prepareGalaxyBuild(WORKSPACE_ROOT, { updateStep, appendLog });
        const { appPath } = prepared;

        // ─── Step 3: Copy to /Applications ─────────────────────────────
        updateStep(3, { status: "running" });

        if (!fs.existsSync(appPath)) {
          throw new Error(`Built app not found at ${appPath}`);
        }

        await stopApp("Galaxy", (line) => appendLog(3, line));

        let needsAdministrator = false;
        try {
          fs.accessSync(path.dirname(destPath), fs.constants.W_OK);
          if (fs.existsSync(destPath)) fs.accessSync(destPath, fs.constants.W_OK);
        } catch {
          needsAdministrator = true;
        }
        const installPrefix = needsAdministrator ? "sudo -n " : "";
        if (needsAdministrator) {
          appendLog(3, "Administrator password required to install in /Applications");
          await runCommandStreaming(
            "sudo -v",
            WORKSPACE_ROOT,
            (line) => appendLog(3, line),
            true
          );
        }

        if (fs.existsSync(destPath)) {
          appendLog(3, `Removing existing ${destPath}`);
          await runCommandStreaming(
            `${installPrefix}rm -rf "${destPath}"`,
            WORKSPACE_ROOT,
            (line) => appendLog(3, line)
          );
        }

        appendLog(3, `Copying ${appPath} → ${destPath}`);
        await runCommandStreaming(
          `${installPrefix}ditto "${appPath}" "${destPath}"`,
          WORKSPACE_ROOT,
          (line) => appendLog(3, line)
        );

        updateStep(3, { status: "done", detail: destPath });

        // ─── Step 4: Launch ────────────────────────────────────────────
        updateStep(4, { status: "running" });
        execSync(`open "${destPath}"`, { stdio: "ignore" });
        updateStep(4, { status: "done" });
      } catch (err: any) {
        process.exitCode = 1;
        setSteps((prev) =>
          prev.map((s) =>
            s.status === "running"
              ? { ...s, status: "error" as StepStatus, detail: err.message }
              : s
          )
        );
      } finally {
        await prepared?.release();
      }
    })();
  }, []);

  const hasError = steps.some((s) => s.status === "error");
  const allDone = hasError || steps.every((s) => s.status === "done");

  return (
    <Box flexDirection="column" padding={1}>
      <Box marginBottom={1}>
        <Text bold color="cyan">
          Build & Install Galaxy → /Applications
        </Text>
      </Box>

      {steps.map((step, i) => (
        <StepLine key={i} step={step} />
      ))}

      {allDone && (
        <Box marginTop={1}>
          {hasError ? (
            <Text color="red" bold>
              ✗ Installation failed.
            </Text>
          ) : (
            <Text color="green" bold>
              ✓ Galaxy installed to /Applications!
            </Text>
          )}
        </Box>
      )}
    </Box>
  );
}

render(<App />);
