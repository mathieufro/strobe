import * as vscode from 'vscode';

// Patterns that identify test markers per language
const TEST_PATTERNS: Array<{ languageIds: string[]; pattern: RegExp }> = [
  // Rust: #[test] or #[tokio::test]
  { languageIds: ['rust'], pattern: /^\s*#\[(?:tokio::)?test\b/ },
  // C++ Catch2: TEST_CASE("name"
  { languageIds: ['cpp', 'c'], pattern: /^\s*TEST_CASE\s*\(/ },
  // C++ GoogleTest: TEST(Suite, Name) or TEST_F(Suite, Name)
  { languageIds: ['cpp', 'c'], pattern: /^\s*TEST(?:_F)?\s*\(/ },
];

// Extract function name from the line after the test attribute
const FN_NAME_PATTERNS: Array<{
  languageIds: string[];
  pattern: RegExp;
}> = [
  // Rust: fn test_name(
  {
    languageIds: ['rust'],
    pattern: /^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)/,
  },
  // Catch2: TEST_CASE("test name"
  { languageIds: ['cpp', 'c'], pattern: /TEST_CASE\s*\(\s*"([^"]+)"/ },
  // GoogleTest: TEST(Suite, Name)
  {
    languageIds: ['cpp', 'c'],
    pattern: /TEST(?:_F)?\s*\(\s*(\w+)\s*,\s*(\w+)/,
  },
];

export class TestCodeLensProvider implements vscode.CodeLensProvider {
  private _onDidChangeCodeLenses = new vscode.EventEmitter<void>();
  readonly onDidChangeCodeLenses = this._onDidChangeCodeLenses.event;

  provideCodeLenses(
    document: vscode.TextDocument,
    _token: vscode.CancellationToken,
  ): vscode.CodeLens[] {
    const lenses: vscode.CodeLens[] = [];
    const langId = document.languageId;

    const testPatterns = TEST_PATTERNS.filter((p) =>
      p.languageIds.includes(langId),
    );
    if (testPatterns.length === 0) return [];

    for (let i = 0; i < document.lineCount; i++) {
      const line = document.lineAt(i).text;

      for (const tp of testPatterns) {
        if (!tp.pattern.test(line)) continue;

        const testName = this.extractTestName(document, i, langId);
        if (!testName) continue;

        const range = new vscode.Range(i, 0, i, line.length);

        lenses.push(
          new vscode.CodeLens(range, {
            title: 'Run Test',
            command: 'strobe.runSingleTest',
            arguments: [testName],
          }),
        );

        lenses.push(
          new vscode.CodeLens(range, {
            title: 'Debug Test',
            command: 'strobe.debugSingleTest',
            arguments: [testName],
          }),
        );

        lenses.push(
          new vscode.CodeLens(range, {
            title: 'Trace',
            command: 'strobe.traceFunction',
            arguments: [testName],
          }),
        );

        break; // Don't match multiple patterns on same line
      }
    }

    return lenses;
  }

  private extractTestName(
    document: vscode.TextDocument,
    markerLine: number,
    langId: string,
  ): string | undefined {
    const fnPatterns = FN_NAME_PATTERNS.filter((p) =>
      p.languageIds.includes(langId),
    );

    if (langId === 'rust') {
      // Rust: test attribute is above the fn declaration — scan down
      for (
        let j = markerLine + 1;
        j < Math.min(markerLine + 4, document.lineCount);
        j++
      ) {
        const text = document.lineAt(j).text;
        for (const fp of fnPatterns) {
          const m = fp.pattern.exec(text);
          if (m) return m[1];
        }
      }
    } else {
      // C++: the test name is on the marker line itself
      const text = document.lineAt(markerLine).text;
      for (const fp of fnPatterns) {
        const m = fp.pattern.exec(text);
        if (m) {
          // GoogleTest: "Suite::Name", Catch2: just the name
          return m[2] ? `${m[1]}::${m[2]}` : m[1];
        }
      }
    }
    return undefined;
  }

  refresh(): void {
    this._onDidChangeCodeLenses.fire();
  }
}
