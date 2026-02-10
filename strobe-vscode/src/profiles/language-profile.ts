export interface LanguageProfile {
  id: string;
  displayName: string;
  filePatterns: string[];
  instrumentationMode: 'native' | 'runtime';
  symbolSource: 'dwarf' | 'runtime';
  patternSeparator: string; // "::" for native, "." for runtime
}

const rustProfile: LanguageProfile = {
  id: 'rust',
  displayName: 'Rust',
  filePatterns: ['*.rs'],
  instrumentationMode: 'native',
  symbolSource: 'dwarf',
  patternSeparator: '::',
};

const cppProfile: LanguageProfile = {
  id: 'cpp',
  displayName: 'C/C++',
  filePatterns: ['*.cpp', '*.cc', '*.c', '*.cxx', '*.h', '*.hpp', '*.hxx'],
  instrumentationMode: 'native',
  symbolSource: 'dwarf',
  patternSeparator: '::',
};

const swiftProfile: LanguageProfile = {
  id: 'swift',
  displayName: 'Swift',
  filePatterns: ['*.swift'],
  instrumentationMode: 'native',
  symbolSource: 'dwarf',
  patternSeparator: '.',
};

const goProfile: LanguageProfile = {
  id: 'go',
  displayName: 'Go',
  filePatterns: ['*.go'],
  instrumentationMode: 'native',
  symbolSource: 'dwarf',
  patternSeparator: '.',
};

export const builtinProfiles: LanguageProfile[] = [
  rustProfile,
  cppProfile,
  swiftProfile,
  goProfile,
];

export function detectProfile(
  languageId: string,
): LanguageProfile | undefined {
  switch (languageId) {
    case 'rust':
      return rustProfile;
    case 'c':
    case 'cpp':
      return cppProfile;
    case 'swift':
      return swiftProfile;
    case 'go':
      return goProfile;
    default:
      return undefined;
  }
}
