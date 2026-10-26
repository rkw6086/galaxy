export interface BuildStepUpdate {
  status?: 'pending' | 'running' | 'done' | 'error';
  detail?: string;
}

export interface BuildObserver {
  updateStep(index: number, update: BuildStepUpdate): void;
  appendLog(index: number, line: string): void;
}

export interface PreparedGalaxyBuild {
  /** Private Galaxy.app copy; safe for consumers to sign without changing the cache. */
  appPath: string;
  reused: boolean;
  identity?: Record<string, unknown>;
  /** Call in a finally block after the app has been copied or packaged. */
  release(): Promise<void>;
}

export function prepareGalaxyBuild(
  workspaceRoot: string,
  observer?: BuildObserver | ((index: number, update: BuildStepUpdate) => void),
): Promise<PreparedGalaxyBuild>;
