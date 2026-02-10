import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

const SETTINGS_PATH = path.join(os.homedir(), '.strobe', 'settings.json');

/** Map from VS Code setting key (without 'strobe.' prefix) to daemon settings.json key */
const SETTING_MAP: Record<string, string> = {
  'events.maxPerSession': 'events.maxPerSession',
  'test.statusRetryMs': 'test.statusRetryMs',
};

export function syncSettingsToDaemon(): void {
  const config = vscode.workspace.getConfiguration('strobe');
  const daemonSettings: Record<string, unknown> = {};

  for (const [shortKey, daemonKey] of Object.entries(SETTING_MAP)) {
    const info = config.inspect(shortKey);
    // Only write values explicitly set by the user (not defaults)
    if (info?.globalValue !== undefined || info?.workspaceValue !== undefined) {
      daemonSettings[daemonKey] = config.get(shortKey);
    }
  }

  if (Object.keys(daemonSettings).length === 0) {
    return;
  }

  try {
    const dir = path.dirname(SETTINGS_PATH);
    fs.mkdirSync(dir, { recursive: true });

    // Merge with existing settings (preserve keys we don't manage)
    let existing: Record<string, unknown> = {};
    try {
      existing = JSON.parse(fs.readFileSync(SETTINGS_PATH, 'utf-8'));
    } catch {
      // File doesn't exist or invalid
    }

    const merged = { ...existing, ...daemonSettings };
    fs.writeFileSync(SETTINGS_PATH, JSON.stringify(merged, null, 2) + '\n');
  } catch {
    // Best-effort — user may not have write permissions
  }
}
