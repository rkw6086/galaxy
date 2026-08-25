#!/usr/bin/env tsx
import React, { useState, useEffect, useCallback } from "react";
import { render, Text, Box } from "ink";
import Spinner from "ink-spinner";
import { spawn, execFileSync, execSync } from "child_process";
import path from "path";
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
  onLog: (line: string) => void
): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, {
      cwd,
      shell: true,
      stdio: ["ignore", "pipe", "pipe"],
    });

    let leftover = "";
    const processChunk = (chunk: Buffer) => {
      const text = leftover + chunk.toString();
      const lines = text.split("\n");
      leftover = lines.pop() || "";
      for (const line of lines) {
        if (line.trim()) onLog(line);
      }
    };

    child.stdout?.on("data", processChunk);
    child.stderr?.on("data", processChunk);

    child.on("close", (code) => {
      if (leftover.trim()) onLog(leftover);
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
    { label: "Build Galaxy", status: "pending" },
    { label: "Copy to /Applications", status: "pending" },
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
      try {
        const appName = "Galaxy.app";
        const appCrateDir = path.join(WORKSPACE_ROOT, "app");
        const bundleDir = path.join(WORKSPACE_ROOT, "target/debug/bundle/osx");
        const appPath = path.join(bundleDir, appName);
        const destPath = path.join("/Applications", appName);

        // ─── Step 0: Build ──────────────────────────────────────────────
        updateStep(0, { status: "running" });

        const buildCmd = "cargo bundle --bin galaxy-oss";

        await runCommandStreaming(buildCmd, appCrateDir, (line) => appendLog(0, line));
        updateStep(0, { status: "done" });

        // ─── Step 1: Copy to /Applications ─────────────────────────────
        updateStep(1, { status: "running" });

        if (!fs.existsSync(appPath)) {
          throw new Error(`Built app not found at ${appPath}`);
        }

        await stopApp("Galaxy", (line) => appendLog(1, line));

        if (fs.existsSync(destPath)) {
          appendLog(1, `Removing existing ${destPath}`);
          execSync(`rm -rf "${destPath}"`, { stdio: "pipe" });
        }

        appendLog(1, `Copying ${appPath} → ${destPath}`);
        await runCommandStreaming(
          `ditto "${appPath}" "${destPath}"`,
          WORKSPACE_ROOT,
          (line) => appendLog(1, line)
        );

        updateStep(1, { status: "done", detail: destPath });

        // ─── Step 2: Launch ────────────────────────────────────────────
        setSteps((prev) => [...prev, { label: "Launch Galaxy", status: "running" }]);
        execSync(`open "${destPath}"`, { stdio: "ignore" });
        setSteps((prev) =>
          prev.map((s, i) => (i === 2 ? { ...s, status: "done" } : s))
        );
      } catch (err: any) {
        setSteps((prev) =>
          prev.map((s) =>
            s.status === "running"
              ? { ...s, status: "error" as StepStatus, detail: err.message }
              : s
          )
        );
      }
    })();
  }, []);

  const allDone = steps.every((s) => s.status === "done" || s.status === "error");
  const hasError = steps.some((s) => s.status === "error");

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
