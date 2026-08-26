#!/usr/bin/env tsx
import React, { useState, useEffect, useCallback } from "react";
import { render, Text, Box } from "ink";
import Spinner from "ink-spinner";
import { spawn, execSync } from "child_process";
import { statSync, readFileSync } from "fs";
import { createHash } from "crypto";
import path from "path";

// ─── Configuration ───────────────────────────────────────────────────────────

const HERMES_USER = process.env.HERMES_USER ?? "ryan";
const HERMES_PASS = process.env.HERMES_PASS;

if (!HERMES_PASS) {
  console.error("Error: HERMES_PASS environment variable is required.");
  process.exit(1);
}

const BASE_URL = "https://client.wst.mini-games.tv";
const UPLOAD_KEY = "wst-data/ryan-share/galaxy/Galaxy.zip";
const INSTALL_SCRIPT_KEY = "wst-data/ryan-share/galaxy/install-galaxy.sh";
const CONTENT_TYPE = "application/zip";
const SCRIPT_CONTENT_TYPE = "text/x-shellscript";

// Workspace root is three levels up from script/build-and-deploy-hermes/src
const WORKSPACE_ROOT = path.resolve(import.meta.dirname, "../../..");

// ─── Types ───────────────────────────────────────────────────────────────────

type StepStatus = "pending" | "running" | "done" | "error";

interface Step {
  label: string;
  status: StepStatus;
  detail?: string;
  logs?: string[];
  progress?: { bytes: number; totalBytes: number };
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

interface StartUploadResponse {
  uploadId: string;
  key: string;
  totalParts: number;
  partSize: number;
  urls: { partNumber: number; url: string }[];
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

const COMMON_HEADERS: Record<string, string> = {
  "User-Agent":
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:152.0) Gecko/20100101 Firefox/152.0",
  Accept: "*/*",
  "Accept-Language": "en-US,en;q=0.9",
  "Content-Type": "application/json",
  Origin: BASE_URL,
  "Sec-GPC": "1",
  Connection: "keep-alive",
  "Sec-Fetch-Dest": "empty",
  "Sec-Fetch-Mode": "cors",
  "Sec-Fetch-Site": "same-origin",
  "Pragma": "no-cache",
  "Cache-Control": "no-cache",
};

function authHeaders(token: string): Record<string, string> {
  return { ...COMMON_HEADERS, Authorization: `Bearer ${token}` };
}

async function apiPost<T>(url: string, body: object, headers: Record<string, string>): Promise<T> {
  const res = await fetch(url, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`POST ${url} failed (${res.status}): ${text}`);
  }
  return res.json() as Promise<T>;
}

function computeFileHash(filePath: string): string {
  const hash = createHash("sha256");
  const data = readFileSync(filePath);
  hash.update(data);
  return hash.digest("hex");
}

async function uploadPart(
  url: string,
  filePath: string,
  partNumber: number,
  partSize: number,
  totalParts: number,
  onProgress: (bytesSent: number) => void
): Promise<string> {
  const fileSize = statSync(filePath).size;
  const start = (partNumber - 1) * partSize;
  const end = partNumber === totalParts ? fileSize : start + partSize;
  const length = end - start;

  // Read the chunk into a buffer
  const { openSync, readSync, closeSync } = await import("fs");
  const fd = openSync(filePath, "r");
  const buffer = Buffer.alloc(length);
  readSync(fd, buffer, 0, length, start);
  closeSync(fd);

  // Stream the upload to track progress
  const CHUNK_SIZE = 256 * 1024; // 256KB reporting chunks
  let uploaded = 0;

  const stream = new ReadableStream({
    start(controller) {
      let offset = 0;
      function push() {
        if (offset >= length) {
          controller.close();
          return;
        }
        const chunk = buffer.subarray(offset, Math.min(offset + CHUNK_SIZE, length));
        controller.enqueue(chunk);
        offset += chunk.length;
        uploaded += chunk.length;
        onProgress(uploaded);
      }
      // Push all chunks synchronously since data is already in memory
      while (offset < length) {
        const chunk = buffer.subarray(offset, Math.min(offset + CHUNK_SIZE, length));
        controller.enqueue(chunk);
        offset += chunk.length;
        uploaded += chunk.length;
        onProgress(uploaded);
      }
      controller.close();
    },
  });

  const res = await fetch(url, {
    method: "PUT",
    headers: {
      "Content-Type": CONTENT_TYPE,
      "Content-Length": String(length),
    },
    body: stream,
    // @ts-ignore - duplex is needed for streaming uploads in Node
    duplex: "half",
  });

  if (!res.ok) {
    const text = await res.text();
    throw new Error(`PUT part ${partNumber} failed (${res.status}): ${text}`);
  }

  const etag = res.headers.get("etag");
  if (!etag) {
    throw new Error(`No ETag returned for part ${partNumber}`);
  }
  return etag;
}

// ─── UI Component ────────────────────────────────────────────────────────────

const BAR_WIDTH = 30;

function ProgressBar({ bytes, totalBytes }: { bytes: number; totalBytes: number }) {
  const pct = Math.min(100, Math.round((bytes / totalBytes) * 100));
  const filled = Math.round((bytes / totalBytes) * BAR_WIDTH);
  const empty = BAR_WIDTH - filled;
  const uploadedMB = (bytes / (1024 * 1024)).toFixed(1);
  const totalMB = (totalBytes / (1024 * 1024)).toFixed(1);
  return (
    <Box marginLeft={3}>
      <Text color="cyan">{'█'.repeat(filled)}</Text>
      <Text color="gray">{'░'.repeat(empty)}</Text>
      <Text color="gray"> {pct}% ({uploadedMB}/{totalMB} MB)</Text>
    </Box>
  );
}

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
      {step.logs && step.logs.length > 0 && !step.progress && (
        <Box flexDirection="column" marginLeft={3}>
          {step.logs.map((line, i) => (
            <Text key={i} color="gray" dimColor>
              {line}
            </Text>
          ))}
        </Box>
      )}
      {step.progress && (
        <ProgressBar bytes={step.progress.bytes} totalBytes={step.progress.totalBytes} />
      )}
    </Box>
  );
}

function App() {
  const [steps, setSteps] = useState<Step[]>([
    { label: "Check working directory", status: "pending" },
    { label: "Pull current branch", status: "pending" },
    { label: "Clean Cargo build artifacts", status: "pending" },
    { label: "Bundle Galaxy", status: "pending" },
    { label: "Create Galaxy.zip", status: "pending" },
    { label: "Authenticate with Hermes", status: "pending" },
    { label: "Start multipart upload", status: "pending" },
    { label: "Upload parts", status: "pending" },
    { label: "Complete upload", status: "pending" },
    { label: "Upload install-galaxy.sh", status: "pending" },
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
        // ─── Step 0: Check working directory ───────────────────────────
        updateStep(0, { status: "running" });
        await runCommandStreaming(
          `if [ -n "$(git status --porcelain)" ]; then git status --short; exit 1; fi`,
          WORKSPACE_ROOT,
          (line) => appendLog(0, line)
        );
        updateStep(0, { status: "done" });

        // ─── Step 1: Pull current branch ────────────────────────────────
        updateStep(1, { status: "running" });
        const branch = execSync("git branch --show-current", {
          cwd: WORKSPACE_ROOT,
          encoding: "utf8",
        }).trim();
        if (!branch) throw new Error("Cannot deploy from a detached HEAD");
        await runCommandStreaming(
          `git pull --ff-only origin "${branch}"`,
          WORKSPACE_ROOT,
          (line) => appendLog(1, line)
        );
        updateStep(1, { status: "done", detail: branch });

        // ─── Step 2: Clean ──────────────────────────────────────────────
        updateStep(2, { status: "running" });
        await runCommandStreaming("cargo clean", WORKSPACE_ROOT, (line) => appendLog(2, line));
        updateStep(2, { status: "done" });

        // ─── Step 3: Bundle ─────────────────────────────────────────────
        updateStep(3, { status: "running" });
        await runCommandStreaming(
          "cargo bundle --bin galaxy-oss --package galaxy",
          WORKSPACE_ROOT,
          (line) => appendLog(3, line)
        );
        updateStep(3, { status: "done" });

        // ─── Step 4: Zip ────────────────────────────────────────────────
        updateStep(4, { status: "running" });

        // Find the .app bundle — cargo bundle outputs to target/debug/bundle/osx/
        const appDir = path.join(
          WORKSPACE_ROOT,
          "target/debug/bundle/osx"
        );
        const zipPath = path.join(WORKSPACE_ROOT, "Galaxy.zip");

        // Remove old zip if exists
        execSync(`rm -f "${zipPath}"`, { stdio: "pipe" });
        // Preserve executable permissions, symlinks, and macOS bundle metadata.
        await runCommandStreaming(
          `ditto -c -k --keepParent "${path.join(appDir, "Galaxy.app")}" "${zipPath}"`,
          WORKSPACE_ROOT,
          (line) => appendLog(4, line)
        );

        const fileSize = statSync(zipPath).size;
        const fileSizeMB = (fileSize / (1024 * 1024)).toFixed(1);
        updateStep(4, { status: "done", detail: `${fileSizeMB} MB` });

        // ─── Step 5: Authenticate ───────────────────────────────────────
        updateStep(5, { status: "running" });

        const loginRes = await apiPost<{ token: string }>(
          `${BASE_URL}/api/auth/login`,
          { username: HERMES_USER, password: HERMES_PASS },
          COMMON_HEADERS
        );
        const token = loginRes.token;
        appendLog(5, `Authenticated as ${HERMES_USER}`);
        updateStep(5, { status: "done" });

        // ─── Step 6: Start upload ───────────────────────────────────────
        updateStep(6, { status: "running" });

        const fileHash = computeFileHash(zipPath);

        const startRes = await apiPost<StartUploadResponse>(
          `${BASE_URL}/api/uploads/start`,
          {
            key: UPLOAD_KEY,
            contentType: CONTENT_TYPE,
            fileSize,
            fileHash,
          },
          authHeaders(token)
        );

        const { uploadId, urls: partUrls, totalParts, partSize } = startRes;
        appendLog(6, `Upload ID: ${uploadId.slice(0, 32)}...`);
        appendLog(6, `File hash: ${fileHash}`);
        appendLog(6, `File size: ${fileSizeMB} MB`);
        appendLog(6, `Parts: ${totalParts}`);
        updateStep(6, { status: "done", detail: `${totalParts} parts` });

        // ─── Step 7: Upload parts ───────────────────────────────────────
        updateStep(7, { status: "running", progress: { bytes: 0, totalBytes: fileSize } });

        // Use the partSize from the server response
        const completedParts: { partNumber: number; etag: string }[] = [];
        let totalBytesUploaded = 0;

        for (const part of partUrls) {
          const prevPartsBytes = totalBytesUploaded;
          const etag = await uploadPart(
            part.url,
            zipPath,
            part.partNumber,
            partSize,
            totalParts,
            (partBytes) => {
              updateStep(7, {
                status: "running",
                progress: { bytes: prevPartsBytes + partBytes, totalBytes: fileSize },
              });
            }
          );

          // Update total bytes for next part's baseline
          const thisPartSize = part.partNumber === totalParts
            ? fileSize - (partSize * (totalParts - 1))
            : partSize;
          totalBytesUploaded += thisPartSize;

          completedParts.push({ partNumber: part.partNumber, etag });

          // Notify server of part completion
          await apiPost(
            `${BASE_URL}/api/uploads/part-complete`,
            {
              uploadId,
              partNumber: part.partNumber,
              etag,
            },
            authHeaders(token)
          );
        }

        updateStep(7, {
          status: "done",
          detail: `${fileSizeMB} MB uploaded`,
          progress: undefined,
        });

        // ─── Step 8: Complete ────────────────────────────────────────────
        updateStep(8, { status: "running" });

        await apiPost(
          `${BASE_URL}/api/uploads/complete`,
          {
            key: UPLOAD_KEY,
            uploadId,
            parts: completedParts.sort((a, b) => a.partNumber - b.partNumber),
          },
          authHeaders(token)
        );

        appendLog(8, `Key: ${UPLOAD_KEY}`);
        updateStep(8, { status: "done" });

        // ─── Step 9: Upload install-galaxy.sh ────────────────────────────
        updateStep(9, { status: "running" });

        const installScriptPath = path.join(WORKSPACE_ROOT, "script", "install-galaxy.sh");
        const scriptFileSize = statSync(installScriptPath).size;
        const scriptFileHash = computeFileHash(installScriptPath);

        const scriptStartRes = await apiPost<StartUploadResponse>(
          `${BASE_URL}/api/uploads/start`,
          {
            key: INSTALL_SCRIPT_KEY,
            contentType: SCRIPT_CONTENT_TYPE,
            fileSize: scriptFileSize,
            fileHash: scriptFileHash,
          },
          authHeaders(token)
        );

        const scriptCompletedParts: { partNumber: number; etag: string }[] = [];
        let scriptBytesUploaded = 0;

        updateStep(9, { status: "running", progress: { bytes: 0, totalBytes: scriptFileSize } });

        for (const part of scriptStartRes.urls) {
          const prevBytes = scriptBytesUploaded;
          const etag = await uploadPart(
            part.url,
            installScriptPath,
            part.partNumber,
            scriptStartRes.partSize,
            scriptStartRes.totalParts,
            (partBytes) => {
              updateStep(9, {
                status: "running",
                progress: { bytes: prevBytes + partBytes, totalBytes: scriptFileSize },
              });
            }
          );

          const thisPartSize = part.partNumber === scriptStartRes.totalParts
            ? scriptFileSize - (scriptStartRes.partSize * (scriptStartRes.totalParts - 1))
            : scriptStartRes.partSize;
          scriptBytesUploaded += thisPartSize;

          scriptCompletedParts.push({ partNumber: part.partNumber, etag });

          await apiPost(
            `${BASE_URL}/api/uploads/part-complete`,
            {
              uploadId: scriptStartRes.uploadId,
              partNumber: part.partNumber,
              etag,
            },
            authHeaders(token)
          );
        }

        await apiPost(
          `${BASE_URL}/api/uploads/complete`,
          {
            key: INSTALL_SCRIPT_KEY,
            uploadId: scriptStartRes.uploadId,
            parts: scriptCompletedParts.sort((a, b) => a.partNumber - b.partNumber),
          },
          authHeaders(token)
        );

        appendLog(9, `Key: ${INSTALL_SCRIPT_KEY}`);
        updateStep(9, { status: "done", detail: `${(scriptFileSize / 1024).toFixed(1)} KB`, progress: undefined });

        // Cleanup zip
        execSync(`rm -f "${zipPath}"`, { stdio: "pipe" });
      } catch (err: any) {
        // Mark current running step as error
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
        <Text bold color="magenta">
          🚀 Build & Deploy Galaxy → Hermes
        </Text>
      </Box>

      {steps.map((step, i) => (
        <StepLine key={i} step={step} />
      ))}

      {allDone && (
        <Box marginTop={1}>
          {hasError ? (
            <Text color="red" bold>
              ✗ Deploy failed.
            </Text>
          ) : (
            <Text color="green" bold>
              ✓ Deploy complete!
            </Text>
          )}
        </Box>
      )}
    </Box>
  );
}

render(<App />);
