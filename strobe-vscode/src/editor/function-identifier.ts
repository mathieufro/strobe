import * as vscode from 'vscode';

export interface IdentifiedFunction {
  name: string;
  containerName?: string;
  range: vscode.Range;
}

/**
 * Identify the function at the cursor position.
 * Primary: VS Code DocumentSymbolProvider (from LSP).
 * Fallback: regex heuristics.
 */
export async function identifyFunctionAtCursor(
  document: vscode.TextDocument,
  position: vscode.Position,
): Promise<IdentifiedFunction | undefined> {
  // Try LSP symbols first
  const symbols = await vscode.commands.executeCommand<
    vscode.DocumentSymbol[]
  >('vscode.executeDocumentSymbolProvider', document.uri);

  if (symbols && symbols.length > 0) {
    const fn = findFunctionContaining(symbols, position);
    if (fn) return fn;
  }

  // Fallback: regex-based identification
  return regexIdentify(document, position);
}

function findFunctionContaining(
  symbols: vscode.DocumentSymbol[],
  position: vscode.Position,
): IdentifiedFunction | undefined {
  for (const sym of symbols) {
    if (sym.range.contains(position)) {
      // Check children first (more specific match)
      if (sym.children.length > 0) {
        const child = findFunctionContaining(sym.children, position);
        if (child) {
          // Prefix with parent name for qualified name
          if (!child.containerName && isContainer(sym)) {
            child.containerName = sym.name;
          }
          return child;
        }
      }
      if (isFunction(sym)) {
        return { name: sym.name, range: sym.range };
      }
    }
  }
  return undefined;
}

function isFunction(sym: vscode.DocumentSymbol): boolean {
  return (
    sym.kind === vscode.SymbolKind.Function ||
    sym.kind === vscode.SymbolKind.Method ||
    sym.kind === vscode.SymbolKind.Constructor
  );
}

function isContainer(sym: vscode.DocumentSymbol): boolean {
  return (
    sym.kind === vscode.SymbolKind.Class ||
    sym.kind === vscode.SymbolKind.Module ||
    sym.kind === vscode.SymbolKind.Namespace ||
    sym.kind === vscode.SymbolKind.Struct
  );
}

// Regex patterns for common function definitions
const FUNCTION_PATTERNS = [
  // Rust: fn name( — handles pub, pub(crate), pub(super), async
  /^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)/,
  // C/C++: ReturnType name( or ReturnType Class::name(
  /^\s*(?:[\w:*&<>]+\s+)+(\w+(?:::\w+)*)\s*\(/,
  // Swift: func name(
  /^\s*(?:public\s+|private\s+|internal\s+|fileprivate\s+|open\s+)?(?:static\s+)?func\s+(\w+)/,
  // Go: func name( or func (r Receiver) name(
  /^\s*func\s+(?:\([^)]*\)\s+)?(\w+)\s*\(/,
];

function regexIdentify(
  document: vscode.TextDocument,
  position: vscode.Position,
): IdentifiedFunction | undefined {
  // Search upward from cursor to find the enclosing function definition
  for (
    let line = position.line;
    line >= Math.max(0, position.line - 50);
    line--
  ) {
    const text = document.lineAt(line).text;
    for (const pattern of FUNCTION_PATTERNS) {
      const match = pattern.exec(text);
      if (match) {
        return {
          name: match[1],
          range: new vscode.Range(line, 0, line, text.length),
        };
      }
    }
  }
  return undefined;
}

/**
 * Format an identified function as a Strobe trace pattern.
 */
export function formatPattern(
  fn: IdentifiedFunction,
  separator: string,
): string {
  if (fn.containerName) {
    return `${fn.containerName}${separator}${fn.name}`;
  }
  // For unqualified names, use wildcard prefix to match any namespace
  return `*${separator}${fn.name}`;
}
