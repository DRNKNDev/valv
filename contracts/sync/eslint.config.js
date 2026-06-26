const privateImportPatterns = ["private/**", "**/private/**"];

module.exports = [
  {
    files: ["src/**/*.ts"],
    rules: {
      "no-restricted-imports": [
        "error",
        {
          patterns: privateImportPatterns
        }
      ]
    }
  }
];
