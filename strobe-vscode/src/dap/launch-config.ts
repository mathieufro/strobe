import { DebugProtocol } from '@vscode/debugprotocol';

export interface StrobeLaunchConfig extends DebugProtocol.LaunchRequestArguments {
  program: string;
  args?: string[];
  cwd?: string;
  env?: Record<string, string>;
  tracePatterns?: string[];
}

export function validateLaunchConfig(config: StrobeLaunchConfig): string | undefined {
  if (!config.program) {
    return 'Missing required field: program';
  }
  return undefined;
}
